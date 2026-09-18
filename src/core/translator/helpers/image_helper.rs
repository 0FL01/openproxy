//! Remote-image fetching for adapters that require inline base64 content.
//!
//! The fetch path retains the existing URL, DNS, redirect, TLS, MIME, and
//! magic-byte checks. C30A adds request-scoped decoded, encoded, and final JSON
//! bounds. C30B binds every validated DNS result to the actual socket connect
//! without replacing the URL hostname used for HTTP Host and TLS verification.

use base64::Engine as _;
use futures_util::StreamExt;
use reqwest::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, LOCATION};
use reqwest::{Client, Response};
use serde::Serialize;
use serde_json::Value;
use std::fmt;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net;
use url::Url;

pub const DEFAULT_MAX_IMAGE_DECODED_BYTES: usize = 10 * 1024 * 1024;
pub const DEFAULT_MAX_IMAGE_AGGREGATE_DECODED_BYTES: usize = 32 * 1024 * 1024;
pub const DEFAULT_MAX_IMAGE_AGGREGATE_ENCODED_BYTES: usize = 48 * 1024 * 1024;
pub const DEFAULT_MAX_IMAGE_REQUEST_BYTES: usize = 64 * 1024 * 1024;

pub const MAX_IMAGE_DECODED_ENV: &str = "OPENPROXY_MAX_IMAGE_DECODED_BYTES";
pub const MAX_IMAGE_AGGREGATE_DECODED_ENV: &str = "OPENPROXY_MAX_IMAGE_AGGREGATE_DECODED_BYTES";
pub const MAX_IMAGE_AGGREGATE_ENCODED_ENV: &str = "OPENPROXY_MAX_IMAGE_AGGREGATE_ENCODED_BYTES";
pub const MAX_IMAGE_REQUEST_ENV: &str = "OPENPROXY_MAX_IMAGE_REQUEST_BYTES";

/// Maximum number of redirect hops before aborting (matches JS `maxRedirects`).
const MAX_REDIRECT_HOPS: usize = 5;
const IMAGE_DNS_TIMEOUT: Duration = Duration::from_secs(5);
const IMAGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IMAGE_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Known image magic-byte prefixes for validation.
const IMAGE_MAGIC_BYTES: &[&[u8]] = &[
    b"\xff\xd8\xff",            // JPEG
    b"\x89PNG\r\n\x1a\n",       // PNG
    b"GIF87a",                  // GIF87a
    b"GIF89a",                  // GIF89a
    b"RIFF",                    // WebP (RIFF....WEBP)
    b"\x00\x00\x01\x00",        // ICO
    b"BM",                      // BMP
    b"\x00\x00\x00\x0c",        // JXL (ISO/IEC 18181)
    b"\x8aMNG\x0d\x0a\x1a\x0a", // MNG
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLimits {
    pub per_image_decoded: usize,
    pub aggregate_decoded: usize,
    pub aggregate_encoded: usize,
    pub final_request: usize,
}

impl ImageLimits {
    pub fn from_env() -> Self {
        Self {
            per_image_decoded: configured_limit(
                MAX_IMAGE_DECODED_ENV,
                DEFAULT_MAX_IMAGE_DECODED_BYTES,
            ),
            aggregate_decoded: configured_limit(
                MAX_IMAGE_AGGREGATE_DECODED_ENV,
                DEFAULT_MAX_IMAGE_AGGREGATE_DECODED_BYTES,
            ),
            aggregate_encoded: configured_limit(
                MAX_IMAGE_AGGREGATE_ENCODED_ENV,
                DEFAULT_MAX_IMAGE_AGGREGATE_ENCODED_BYTES,
            ),
            // Image expansion must never exceed the existing C27 prepared-body
            // ceiling, even if an environment override is accidentally larger.
            final_request: configured_limit(MAX_IMAGE_REQUEST_ENV, DEFAULT_MAX_IMAGE_REQUEST_BYTES)
                .min(DEFAULT_MAX_IMAGE_REQUEST_BYTES),
        }
    }
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self::from_env()
    }
}

fn configured_limit(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImagePrefetchError {
    InvalidAttachment(&'static str),
    InvalidUrl,
    BlockedDestination,
    Transport(String),
    HttpStatus(u16),
    InvalidMime,
    InvalidMagic,
    DeclaredImageTooLarge { declared: u64, limit: usize },
    ImageTooLarge { limit: usize },
    AggregateDecodedTooLarge { limit: usize },
    AggregateEncodedTooLarge { limit: usize },
    FinalRequestTooLarge { limit: usize },
    Capacity { limit: usize },
}

impl ImagePrefetchError {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::DeclaredImageTooLarge { .. }
            | Self::ImageTooLarge { .. }
            | Self::AggregateDecodedTooLarge { .. }
            | Self::AggregateEncodedTooLarge { .. }
            | Self::FinalRequestTooLarge { .. }
            | Self::Capacity { .. } => 413,
            _ => 502,
        }
    }
}

impl fmt::Display for ImagePrefetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAttachment(message) => {
                write!(formatter, "invalid remote image attachment: {message}")
            }
            Self::InvalidUrl => formatter.write_str("invalid remote image URL"),
            Self::BlockedDestination => {
                formatter.write_str("remote image destination is not public")
            }
            Self::Transport(message) => {
                write!(formatter, "remote image transport failed: {message}")
            }
            Self::HttpStatus(status) => write!(formatter, "remote image returned HTTP {status}"),
            Self::InvalidMime => formatter.write_str("remote image has an invalid content type"),
            Self::InvalidMagic => formatter.write_str("remote image has invalid image bytes"),
            Self::DeclaredImageTooLarge { declared, limit } => write!(
                formatter,
                "remote image declared {declared} decoded bytes, exceeding the {limit}-byte limit"
            ),
            Self::ImageTooLarge { limit } => write!(
                formatter,
                "remote image exceeded the {limit}-byte decoded limit"
            ),
            Self::AggregateDecodedTooLarge { limit } => write!(
                formatter,
                "remote images exceeded the {limit}-byte aggregate decoded limit"
            ),
            Self::AggregateEncodedTooLarge { limit } => write!(
                formatter,
                "remote images exceeded the {limit}-byte aggregate encoded limit"
            ),
            Self::FinalRequestTooLarge { limit } => write!(
                formatter,
                "image expansion would exceed the {limit}-byte final request limit"
            ),
            Self::Capacity { limit } => write!(
                formatter,
                "unable to allocate within the {limit}-byte image budget"
            ),
        }
    }
}

impl std::error::Error for ImagePrefetchError {}

#[derive(Debug)]
struct DownloadedImage {
    bytes: Vec<u8>,
    mime_type: String,
}

/// Outcome of a successful fetch. The data URL is built directly into one
/// pre-sized string; no intermediate base64 string is allocated.
#[derive(Debug, Clone)]
pub struct FetchedImage {
    pub data_url: String,
    pub mime_type: String,
    data_offset: usize,
}

impl FetchedImage {
    /// Convert the already allocated data URL buffer into the raw Claude
    /// base64 payload without allocating another encoded representation.
    pub fn into_claude_source(mut self) -> (String, String) {
        self.data_url.drain(..self.data_offset);
        (self.mime_type, self.data_url)
    }
}

#[derive(Debug)]
pub struct ImagePrefetchBudget {
    limits: ImageLimits,
    decoded: usize,
    encoded: usize,
    initial_request: usize,
}

impl ImagePrefetchBudget {
    pub fn new(body: &Value) -> Result<Self, ImagePrefetchError> {
        Self::with_limits(body, ImageLimits::from_env())
    }

    #[doc(hidden)]
    pub fn with_limits(body: &Value, limits: ImageLimits) -> Result<Self, ImagePrefetchError> {
        let initial_request = serialized_len_bounded(body, limits.final_request)?;
        Ok(Self {
            limits,
            decoded: 0,
            encoded: 0,
            initial_request,
        })
    }

    fn read_limits(&self) -> ImageReadLimits {
        ImageReadLimits {
            per_image: self.limits.per_image_decoded,
            aggregate_start: self.decoded,
            aggregate: self.limits.aggregate_decoded,
        }
    }

    fn commit_decoded(&mut self, decoded: usize) -> Result<(), ImagePrefetchError> {
        self.decoded = self.decoded.checked_add(decoded).ok_or(
            ImagePrefetchError::AggregateDecodedTooLarge {
                limit: self.limits.aggregate_decoded,
            },
        )?;
        if self.decoded > self.limits.aggregate_decoded {
            return Err(ImagePrefetchError::AggregateDecodedTooLarge {
                limit: self.limits.aggregate_decoded,
            });
        }
        Ok(())
    }

    fn reserve_encoded(&mut self, encoded_len: usize) -> Result<(), ImagePrefetchError> {
        let next_encoded = self.encoded.checked_add(encoded_len).ok_or(
            ImagePrefetchError::AggregateEncodedTooLarge {
                limit: self.limits.aggregate_encoded,
            },
        )?;
        if next_encoded > self.limits.aggregate_encoded {
            return Err(ImagePrefetchError::AggregateEncodedTooLarge {
                limit: self.limits.aggregate_encoded,
            });
        }
        // This estimate is intentionally conservative: retained remote URL
        // bytes are not subtracted before their replacement. It is checked
        // before allocating encoded output, then the exact JSON is checked
        // after mutation/translation.
        let projected = self.initial_request.checked_add(next_encoded).ok_or(
            ImagePrefetchError::FinalRequestTooLarge {
                limit: self.limits.final_request,
            },
        )?;
        if projected > self.limits.final_request {
            return Err(ImagePrefetchError::FinalRequestTooLarge {
                limit: self.limits.final_request,
            });
        }
        self.encoded = next_encoded;
        Ok(())
    }

    pub fn account_existing_data_url(&mut self, data_url: &str) -> Result<(), ImagePrefetchError> {
        self.account_existing_encoded_len(data_url.len())
    }

    pub fn account_existing_encoded_len(
        &mut self,
        encoded_len: usize,
    ) -> Result<(), ImagePrefetchError> {
        self.encoded = self.encoded.checked_add(encoded_len).ok_or(
            ImagePrefetchError::AggregateEncodedTooLarge {
                limit: self.limits.aggregate_encoded,
            },
        )?;
        if self.encoded > self.limits.aggregate_encoded {
            return Err(ImagePrefetchError::AggregateEncodedTooLarge {
                limit: self.limits.aggregate_encoded,
            });
        }
        Ok(())
    }

    pub fn ensure_final_request(&self, body: &Value) -> Result<(), ImagePrefetchError> {
        serialized_len_bounded(body, self.limits.final_request).map(|_| ())
    }
}

#[derive(Debug, Clone, Copy)]
struct ImageReadLimits {
    per_image: usize,
    aggregate_start: usize,
    aggregate: usize,
}

/// Returns `true` if `ip` is in a private or reserved range that should not
/// be reachable from the image fetcher.
fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 10
                || o[0] == 127
                || (o[0] == 172 && (o[1] & 0xF0) == 0x10)
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 169 && o[1] == 254)
                || (o[0] == 100 && (o[1] & 0xC0) == 0x40)
                || v4.is_multicast()
                || v4.is_broadcast()
                || (o[0] >= 240)
                || o[0] == 0
        }
        IpAddr::V6(v6) => {
            let o = v6.octets();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique-local (fc00::/7).
                || (o[0] & 0xfe) == 0xfc
                // Link-local (fe80::/10).
                || (o[0] == 0xfe && (o[1] & 0xc0) == 0x80)
                || (o[..12] == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff]
                    && is_private_ip(IpAddr::V4(Ipv4Addr::new(o[12], o[13], o[14], o[15]))))
        }
    }
}

fn is_valid_image_body(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    IMAGE_MAGIC_BYTES
        .iter()
        .any(|magic| bytes.starts_with(magic))
}

#[derive(Debug, Clone)]
struct ValidatedImageTarget {
    host: String,
    /// Empty only when the URL host itself is a validated IP literal.
    pinned_addrs: Vec<SocketAddr>,
}

async fn resolve_image_target(url: &Url) -> Result<ValidatedImageTarget, ImagePrefetchError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ImagePrefetchError::InvalidUrl);
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or(ImagePrefetchError::InvalidUrl)?
        .to_ascii_lowercase();

    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(ip) {
            return Err(ImagePrefetchError::BlockedDestination);
        }
        return Ok(ValidatedImageTarget {
            host,
            pinned_addrs: Vec::new(),
        });
    }

    let resolved = tokio::time::timeout(IMAGE_DNS_TIMEOUT, net::lookup_host((host.as_str(), 0)))
        .await
        .map_err(|_| ImagePrefetchError::BlockedDestination)?
        .map_err(|_| ImagePrefetchError::BlockedDestination)?;
    let mut pinned_addrs: Vec<SocketAddr> = Vec::new();
    for addr in resolved {
        let ip = addr.ip();
        if !is_private_ip(ip) && !pinned_addrs.iter().any(|existing| existing.ip() == ip) {
            // Reqwest documents that port zero uses the URL's scheme/default
            // port and that an explicit URL port always wins.
            pinned_addrs.push(SocketAddr::new(ip, 0));
        }
    }
    if pinned_addrs.is_empty() {
        return Err(ImagePrefetchError::BlockedDestination);
    }

    Ok(ValidatedImageTarget { host, pinned_addrs })
}

fn build_pinned_image_client(target: &ValidatedImageTarget) -> Result<Client, ImagePrefetchError> {
    pinned_image_client_builder(target)
        .build()
        .map_err(|error| ImagePrefetchError::Transport(error.to_string()))
}

fn pinned_image_client_builder(target: &ValidatedImageTarget) -> reqwest::ClientBuilder {
    let mut builder = reqwest::Client::builder()
        .timeout(IMAGE_REQUEST_TIMEOUT)
        .connect_timeout(IMAGE_CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        // A system proxy can resolve the hostname again and bypass the pinned
        // address set. Required inline image fetches are intentionally direct.
        .no_proxy();
    if !target.pinned_addrs.is_empty() {
        builder = builder.resolve_to_addrs(&target.host, &target.pinned_addrs);
    }
    builder
}

pub async fn fetch_image_as_base64(
    _client: &Client,
    image_url: &str,
    budget: &mut ImagePrefetchBudget,
) -> Result<FetchedImage, ImagePrefetchError> {
    if !image_url.starts_with("http://") && !image_url.starts_with("https://") {
        return Err(ImagePrefetchError::InvalidUrl);
    }

    let mut current_url = image_url.to_string();
    let mut hop: usize = 0;
    let response = loop {
        let parsed = Url::parse(&current_url).map_err(|_| ImagePrefetchError::InvalidUrl)?;
        let target = resolve_image_target(&parsed).await?;
        let pinned_client = build_pinned_image_client(&target)?;
        let response = pinned_client
            .get(parsed)
            .send()
            .await
            .map_err(|error| ImagePrefetchError::Transport(error.to_string()))?;

        let status = response.status();
        let location = if status.is_redirection() {
            response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
        } else {
            None
        };
        let Some(location) = location else {
            break response;
        };

        if hop >= MAX_REDIRECT_HOPS {
            return Err(ImagePrefetchError::Transport(
                "remote image redirect limit exceeded".into(),
            ));
        }

        let next_url = match Url::parse(location) {
            Ok(absolute) => absolute.to_string(),
            Err(_) => Url::parse(&current_url)
                .map_err(|_| ImagePrefetchError::InvalidUrl)?
                .join(location)
                .map_err(|_| ImagePrefetchError::InvalidUrl)?
                .to_string(),
        };

        hop += 1;
        current_url = next_url;
    };

    let downloaded = read_image_response(response, budget.read_limits()).await?;
    budget.commit_decoded(downloaded.bytes.len())?;
    encode_image(downloaded, budget)
}

async fn read_image_response(
    response: Response,
    limits: ImageReadLimits,
) -> Result<DownloadedImage, ImagePrefetchError> {
    if !response.status().is_success() {
        return Err(ImagePrefetchError::HttpStatus(response.status().as_u16()));
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !content_type.starts_with("image/") && !content_type.is_empty() {
        return Err(ImagePrefetchError::InvalidMime);
    }

    if declared_length_is_identity_encoded(&response) {
        if let Some(declared) = declared_length(&response) {
            if declared > limits.per_image as u64 {
                return Err(ImagePrefetchError::DeclaredImageTooLarge {
                    declared,
                    limit: limits.per_image,
                });
            }
            let aggregate = (limits.aggregate_start as u64)
                .checked_add(declared)
                .ok_or(ImagePrefetchError::AggregateDecodedTooLarge {
                    limit: limits.aggregate,
                })?;
            if aggregate > limits.aggregate as u64 {
                return Err(ImagePrefetchError::AggregateDecodedTooLarge {
                    limit: limits.aggregate,
                });
            }
        }
    }

    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ImagePrefetchError::Transport(error.to_string()))?;
        let next_image =
            bytes
                .len()
                .checked_add(chunk.len())
                .ok_or(ImagePrefetchError::ImageTooLarge {
                    limit: limits.per_image,
                })?;
        if next_image > limits.per_image {
            return Err(ImagePrefetchError::ImageTooLarge {
                limit: limits.per_image,
            });
        }
        let next_aggregate = limits.aggregate_start.checked_add(next_image).ok_or(
            ImagePrefetchError::AggregateDecodedTooLarge {
                limit: limits.aggregate,
            },
        )?;
        if next_aggregate > limits.aggregate {
            return Err(ImagePrefetchError::AggregateDecodedTooLarge {
                limit: limits.aggregate,
            });
        }
        bytes
            .try_reserve(chunk.len())
            .map_err(|_| ImagePrefetchError::Capacity {
                limit: limits.per_image.min(limits.aggregate),
            })?;
        bytes.extend_from_slice(&chunk);
    }

    if !is_valid_image_body(&bytes) {
        return Err(ImagePrefetchError::InvalidMagic);
    }

    let mime_type = if content_type.starts_with("image/") {
        content_type
    } else {
        infer_mime_from_magic(&bytes)
    };
    Ok(DownloadedImage { bytes, mime_type })
}

/// Narrow response-injection seam for deterministic C30A tests. It starts
/// after URL/DNS/connect policy so loopback fixtures cannot weaken production
/// SSRF validation. Production callers must use `fetch_image_as_base64`.
#[doc(hidden)]
pub async fn process_image_response_for_test(
    response: Response,
    budget: &mut ImagePrefetchBudget,
) -> Result<FetchedImage, ImagePrefetchError> {
    let downloaded = read_image_response(response, budget.read_limits()).await?;
    budget.commit_decoded(downloaded.bytes.len())?;
    encode_image(downloaded, budget)
}

fn encode_image(
    image: DownloadedImage,
    budget: &mut ImagePrefetchBudget,
) -> Result<FetchedImage, ImagePrefetchError> {
    let encoded_len = base64_encoded_len(image.bytes.len())?;
    let data_offset = 5usize
        .checked_add(image.mime_type.len())
        .and_then(|value| value.checked_add(8))
        .ok_or(ImagePrefetchError::AggregateEncodedTooLarge {
            limit: budget.limits.aggregate_encoded,
        })?;
    let data_url_len = data_offset.checked_add(encoded_len).ok_or(
        ImagePrefetchError::AggregateEncodedTooLarge {
            limit: budget.limits.aggregate_encoded,
        },
    )?;
    budget.reserve_encoded(data_url_len)?;

    let mut data_url = String::new();
    data_url
        .try_reserve_exact(data_url_len)
        .map_err(|_| ImagePrefetchError::Capacity {
            limit: budget.limits.aggregate_encoded,
        })?;
    data_url.push_str("data:");
    data_url.push_str(&image.mime_type);
    data_url.push_str(";base64,");
    base64::engine::general_purpose::STANDARD.encode_string(&image.bytes, &mut data_url);
    debug_assert_eq!(data_url.len(), data_url_len);

    Ok(FetchedImage {
        data_url,
        mime_type: image.mime_type,
        data_offset,
    })
}

fn base64_encoded_len(decoded_len: usize) -> Result<usize, ImagePrefetchError> {
    let complete = decoded_len / 3;
    let remainder = decoded_len % 3;
    complete
        .checked_mul(4)
        .and_then(|value| value.checked_add(usize::from(remainder != 0) * 4))
        .ok_or(ImagePrefetchError::AggregateEncodedTooLarge { limit: usize::MAX })
}

pub fn ensure_final_request_size(body: &Value) -> Result<(), ImagePrefetchError> {
    serialized_len_bounded(body, ImageLimits::from_env().final_request).map(|_| ())
}

struct CountingWriter {
    written: usize,
    limit: usize,
    exceeded: bool,
}

impl Write for CountingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let next = self.written.checked_add(buffer.len()).ok_or_else(|| {
            self.exceeded = true;
            io::Error::other("serialized request length overflow")
        })?;
        if next > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("serialized request exceeds limit"));
        }
        self.written = next;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_len_bounded<T: Serialize>(
    value: &T,
    limit: usize,
) -> Result<usize, ImagePrefetchError> {
    let mut writer = CountingWriter {
        written: 0,
        limit,
        exceeded: false,
    };
    if serde_json::to_writer(&mut writer, value).is_err() {
        return Err(if writer.exceeded {
            ImagePrefetchError::FinalRequestTooLarge { limit }
        } else {
            ImagePrefetchError::InvalidAttachment("request cannot be serialized")
        });
    }
    Ok(writer.written)
}

fn declared_length(response: &Response) -> Option<u64> {
    response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

fn declared_length_is_identity_encoded(response: &Response) -> bool {
    response
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| value.trim().is_empty() || value.eq_ignore_ascii_case("identity"))
}

fn infer_mime_from_magic(bytes: &[u8]) -> String {
    if bytes.starts_with(b"\xff\xd8\xff") {
        return "image/jpeg".into();
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "image/png".into();
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return "image/gif".into();
    }
    if bytes.starts_with(b"RIFF") {
        return "image/webp".into();
    }
    if bytes.starts_with(b"BM") {
        return "image/bmp".into();
    }
    if bytes.starts_with(b"\x00\x00\x01\x00") {
        return "image/x-icon".into();
    }
    "image/jpeg".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    // Non-secret, self-signed test-only certificate/key for image.test.
    const TEST_ROOT_DER_B64: &str = "MIIDJTCCAg2gAwIBAgIURb/jtt0wjp1H54/sU0Duv3Di3eowDQYJKoZIhvcNAQELBQAwGTEXMBUGA1UEAwwOQzMwQiBUZXN0IFJvb3QwIBcNMjYwOTE4MDI0OTM2WhgPMjEyNjA4MjUwMjQ5MzZaMBkxFzAVBgNVBAMMDkMzMEIgVGVzdCBSb290MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA54+5giTc6Zp8Exrp8/RwSjpuDVWwzafH1kU14g5cJrTrmAB3w1deS/ibMqfFjMP7HIThnnMPB5pWtO3EtSnvrL5HH7ZfeqIfeLbhRcwr7+DqFbzTbupEADrf9WyYjRPebZb3tZfLtuudrZYpNGw0NaQLzkLTfexHNQthEePtfUQGHPVeavigOIQGFNmGX+OvrEXRZtJ0hzZMaipO7OZJPpX2iHgd9MGW+cPx/FR+GSi4HcAyWy4dFjMGTrDzVym5o3Trl7SEPWKA3YTnxHO3CEdcC5ElIarS2xxreRv/im34RNLEzqSE6YsahXry84FJ5RBxqkj0cLKNYmDRo1FUWwIDAQABo2MwYTAdBgNVHQ4EFgQU/fNJb/5IwFH9XjinK5maH6Blm1wwHwYDVR0jBBgwFoAU/fNJb/5IwFH9XjinK5maH6Blm1wwDwYDVR0TAQH/BAUwAwEB/zAOBgNVHQ8BAf8EBAMCAQYwDQYJKoZIhvcNAQELBQADggEBAHfw9BUHpanUJn5HVx42ycskfHd2SACZ/azTkDU+0Axcl/oF4VKaWp0LmkI3dOga7CSckac8zNAxL7ciyHNCqjgv/0WppZltLhutCesar+LTHF1MrvQj94GAHimkdj/RUfb0KX/lRyGgkUlIyUcd3xoUC7YDFJHzKBEMvi/pNW76zAB8HxJhedJcnhaG8Wh00RRoEMXXxBm382shAUsn3P78UjPM4TuHnQX1PRG4Zrn3Nj6p1BEEarQLU6JME2V3IC+6Mj+3ppU80bDVZebMnxgSIJmvgP5zSzRYmw99ujlgBa9zw1uAdHFUaICZrWuMWw5eS7pA6ENZJ0xOjDIJUUw=";
    const TEST_CERT_DER_B64: &str = "MIIDTDCCAjSgAwIBAgIUU9IFOrZ6Ohj5KaJoH/oCiFuxjBswDQYJKoZIhvcNAQELBQAwGTEXMBUGA1UEAwwOQzMwQiBUZXN0IFJvb3QwIBcNMjYwOTE4MDI0OTM2WhgPMjEyNjA4MjUwMjQ5MzZaMBUxEzARBgNVBAMMCmltYWdlLnRlc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQCy1i1BmlntehbYtrv42VTW7PEFpYQedVRbdREu/Fa04DmQOaLi6ooeYiPwmAdDSeTQHaXDyeXo0b7EZV9JFTRQcZEEUw4kjIyO/0C3K9Xfx5Cti0jW4GOKNMX0zjAywMq7zdNeJ3lYasui7EiGkwrgpXoEIZQJM6eNy5P7/MRw40eeOUJH2ge42p7foCvR8UYKOZwKd2u/ojOYb2pIAc+LUOwCvjv9WWHcohVOcGdo/kkdzOcBhtEt1dolAVE66cGChN5tibjfh5fi54TN5dWraJQZ4511/z46UarGdB21MiRVXeUeHDlzIdjqF8hhRZx1LcfyTzDtjGi5a4RYkgJLAgMBAAGjgY0wgYowDAYDVR0TAQH/BAIwADAOBgNVHQ8BAf8EBAMCBaAwEwYDVR0lBAwwCgYIKwYBBQUHAwEwFQYDVR0RBA4wDIIKaW1hZ2UudGVzdDAdBgNVHQ4EFgQUIjZ0kSUzpm3F79JPHzqKj3d+2GcwHwYDVR0jBBgwFoAU/fNJb/5IwFH9XjinK5maH6Blm1wwDQYJKoZIhvcNAQELBQADggEBAMk8VOzOnA6C9vYIeqHR5Lte4YxMC2fVzxK9rsnTbEnttaNugx7we6ecitLb2E+EgNY+8bcFI9R50ecy9WjS7k2gXAMEIQnTf83953S8wBQtbuqIJITCoffE4qkbmPL9J3ILGawGOM9ZvyQxWzM5s9RIy6zBs7LgFXQpJQvZ4H7EWMLfCOvMFj0NlHCRNZf8tGEtVdpx6K8ImRjHDCRSAycnc2mJgZAJ5kwOyBUQbYWPem+RPIM1iifR7SHYS10EAlO+8zgXUFIP2iN/lj4arzHC9h8hxlzMzY/7ePfqd9HMbib1nq2X4QbTOypqn6YibUfnjBDA25AoDdTgs6eibH8=";
    const TEST_KEY_DER_B64: &str = "MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCy1i1BmlntehbYtrv42VTW7PEFpYQedVRbdREu/Fa04DmQOaLi6ooeYiPwmAdDSeTQHaXDyeXo0b7EZV9JFTRQcZEEUw4kjIyO/0C3K9Xfx5Cti0jW4GOKNMX0zjAywMq7zdNeJ3lYasui7EiGkwrgpXoEIZQJM6eNy5P7/MRw40eeOUJH2ge42p7foCvR8UYKOZwKd2u/ojOYb2pIAc+LUOwCvjv9WWHcohVOcGdo/kkdzOcBhtEt1dolAVE66cGChN5tibjfh5fi54TN5dWraJQZ4511/z46UarGdB21MiRVXeUeHDlzIdjqF8hhRZx1LcfyTzDtjGi5a4RYkgJLAgMBAAECggEAL1Olu2A9Hy75n9VQDm15Wb1vlVZ1jdfwAJoM95m42noiUawnEpUOAzdmmnOpLGIARyEpbpReHwxuxyuqmT0e0JEVVwchzYNSaT9coXzcxzFZdMvQ9WiBfyAzKPNt5uiFXkj8gETDBKaSiGWuVcrRDJ1OGozGT98XHtu6qGt+kv7osq7+KuDSJV/GtiGk6VxFXJ0mJQAExB3Bjb2bnFerSEm9ji/OKxbL0dWFPWYPH1DwqkDH9LUUuX6ISCfR+VnmhQdHHdZKMxHTO7lPxTbH4OMTVfrTAYXAX1cfbimiEArkdRhZgcLlG6vhqKuJ6JGdQ3npQ/pfEFwoVkTs//b+jQKBgQDxQXNe8zyguhDQKidz+psDvNUG9TKgThQcIL1i9//movTn/fxiEEBm0iEGtK2qpqckT7LuGCwjnugQYTsvACu+6okqX4OhjiHJW3AG+tcDVLg9Iby1ghB3it9gVMgippZMSSuViVQ/cJUNEFBlg4rXzihBw3m254lmIX+FtY2OJwKBgQC9xCc6kyXv8ORy831/+s3NNVkHRMfVy87pBB4lAvQA5Ot9o6+2cXxV3GT7OJ/UCdUS6ZQ9CgTRFBoy2ZECLKIKbr7DHcKL0V8Z2gWg5w7GpzVb1QbeUufkUNp3iIrY5gSR+NVgdVAlEq0Sol9DkncOd/3ndO4JohCM0AIWWAilPQKBgQCJpd49fmpJCeAt5N89vO5U45hnr96CuAmhQszLetP2s2MoOjGVgdA82gcd3Fh51Tvn69EbJ4+Hg1LEhbVOy7op44b/Nh/UPVPpntH8KYIj1GKJ/oW8yci7a/Cm9bh6jLGUNOs5FnGLAKEKIxS71qGRH364ht8bDF/IdbRchYmOYQKBgEYnUelRpODDSXFFmsvJSHPomPUa9FWRdvil0Q8VvGboOjbEYJ0t7y0+wQYutMeKqv7G40p9fbJdoCHUVMtc/Lqmnlc7lURjLqk79IC7arb98bf1VQiz1Y6OZXVzQQFWZ7IfL6LtO6e488rDxDwS1Xi/21nJ3IYrJ7sm2t6vxOJxAoGBAKyXC+Rp4pgTzctmKqmmYvpJgbGIBD47L23rbMPfwfk4Qcui5LMnRDNiiHBBbnIG9EMAdSH7DR96yJcgB0oeNIjPGx50dGcDfOyzoRHLkazR+AtctHzR954G+dqd0P/YtP4j9ODe6J6F0KcS4Ujmf1/jmtqavjBp40ioV/XhGd1l";

    struct RebindingResolver {
        calls: AtomicUsize,
    }

    impl reqwest::dns::Resolve for RebindingResolver {
        fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let private = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0);
            let addrs: reqwest::dns::Addrs = Box::new(std::iter::once(private));
            Box::pin(std::future::ready(Ok(addrs)))
        }
    }

    #[test]
    fn detects_private_addresses_without_weakening_ssrf() {
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1))));
        assert!(is_private_ip(IpAddr::V6("::1".parse().unwrap())));
        assert!(is_private_ip(IpAddr::V6(
            "::ffff:10.0.0.1".parse().unwrap()
        )));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1))));
        assert!(is_private_ip(IpAddr::V6("::".parse().unwrap())));
        assert!(is_private_ip(IpAddr::V6("fe80::1".parse().unwrap())));
        assert!(is_private_ip(IpAddr::V6("fc00::1".parse().unwrap())));
        assert!(is_private_ip(IpAddr::V6("ff02::1".parse().unwrap())));
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_private_ip(IpAddr::V6(
            "::ffff:8.8.8.8".parse().unwrap()
        )));
    }

    #[tokio::test]
    async fn private_literal_targets_are_blocked_on_every_hop() {
        for url in [
            "http://127.0.0.1/image",
            "http://2130706433/image",
            "http://10.0.0.1/image",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/image",
            "http://[fe80::1]/image",
            "http://[fc00::1]/image",
            "http://[::ffff:127.0.0.1]/image",
        ] {
            let parsed = Url::parse(url).unwrap();
            assert!(
                matches!(
                    resolve_image_target(&parsed).await,
                    Err(ImagePrefetchError::BlockedDestination)
                ),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn pinned_client_connects_only_to_supplied_addresses_and_preserves_host() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 4096];
            let read = socket.read(&mut buffer).await.unwrap();
            buffer.truncate(read);
            let _ = request_tx.send(String::from_utf8_lossy(&buffer).into_owned());
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 8\r\nConnection: close\r\n\r\n\x89PNG\r\n\x1a\n",
                )
                .await
                .unwrap();
        });

        let target = ValidatedImageTarget {
            host: "c30b.invalid".into(),
            // Both candidates are passed to the connector. The first has no
            // listener; the second is the only possible successful socket.
            pinned_addrs: vec![
                SocketAddr::new("127.0.0.2".parse().unwrap(), 0),
                SocketAddr::new("127.0.0.1".parse().unwrap(), 0),
            ],
        };
        let resolver = Arc::new(RebindingResolver {
            calls: AtomicUsize::new(0),
        });
        let response = pinned_image_client_builder(&target)
            .dns_resolver(resolver.clone())
            .build()
            .unwrap()
            .get(format!("http://c30b.invalid:{}/image", address.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let request = request_rx.await.unwrap().to_ascii_lowercase();
        assert!(request.contains(&format!("host: c30b.invalid:{}", address.port())));
        assert_eq!(
            resolver.calls.load(Ordering::SeqCst),
            0,
            "the pinned override must prevent a second, rebinding DNS lookup"
        );
        server.await.unwrap();
    }

    fn spawn_test_tls_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert_der = base64::engine::general_purpose::STANDARD
            .decode(TEST_CERT_DER_B64)
            .unwrap();
        let key_der = base64::engine::general_purpose::STANDARD
            .decode(TEST_KEY_DER_B64)
            .unwrap();
        let cert = rustls::pki_types::CertificateDer::from(cert_der);
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(key_der).into();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::task::spawn_blocking(move || {
            let (socket, _) = listener.accept().unwrap();
            let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
            let mut tls = rustls::StreamOwned::new(connection, socket);
            let mut request = [0u8; 4096];
            if tls.read(&mut request).is_ok() {
                let _ = tls.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 8\r\nConnection: close\r\n\r\n\x89PNG\r\n\x1a\n",
                );
                let _ = tls.flush();
            }
        });
        (address, task)
    }

    #[tokio::test]
    async fn pinned_tls_keeps_hostname_verification_enabled() {
        let root_der = base64::engine::general_purpose::STANDARD
            .decode(TEST_ROOT_DER_B64)
            .unwrap();
        let root = reqwest::Certificate::from_der(&root_der).unwrap();

        let (good_address, good_server) = spawn_test_tls_server();
        let good_target = ValidatedImageTarget {
            host: "image.test".into(),
            pinned_addrs: vec![SocketAddr::new("127.0.0.1".parse().unwrap(), 0)],
        };
        let good = pinned_image_client_builder(&good_target)
            .add_root_certificate(root.clone())
            .build()
            .unwrap()
            .get(format!("https://image.test:{}/image", good_address.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(good.status(), reqwest::StatusCode::OK);
        good_server.await.unwrap();

        let (bad_address, bad_server) = spawn_test_tls_server();
        let bad_target = ValidatedImageTarget {
            host: "wrong.test".into(),
            pinned_addrs: vec![SocketAddr::new("127.0.0.1".parse().unwrap(), 0)],
        };
        let error = pinned_image_client_builder(&bad_target)
            .add_root_certificate(root)
            .build()
            .unwrap()
            .get(format!("https://wrong.test:{}/image", bad_address.port()))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_connect(), "unexpected TLS error: {error}");
        bad_server.await.unwrap();
    }

    #[test]
    fn validates_magic_and_infers_mime() {
        assert!(is_valid_image_body(b"\xff\xd8\xff\x00"));
        assert!(is_valid_image_body(b"\x89PNG\r\n\x1a\n..."));
        assert!(is_valid_image_body(b"GIF89a..."));
        assert!(is_valid_image_body(b"RIFF....WEBP"));
        assert!(!is_valid_image_body(b"<!DOCTYPE html>"));
        assert!(!is_valid_image_body(b""));
        assert_eq!(infer_mime_from_magic(b"\xff\xd8\xff"), "image/jpeg");
        assert_eq!(infer_mime_from_magic(b"\x89PNG\r\n\x1a\n"), "image/png");
        assert_eq!(infer_mime_from_magic(b"GIF87a"), "image/gif");
    }

    #[test]
    fn base64_length_is_exact_and_checked() {
        assert_eq!(base64_encoded_len(0).unwrap(), 0);
        assert_eq!(base64_encoded_len(1).unwrap(), 4);
        assert_eq!(base64_encoded_len(2).unwrap(), 4);
        assert_eq!(base64_encoded_len(3).unwrap(), 4);
        assert_eq!(base64_encoded_len(4).unwrap(), 8);
        assert!(base64_encoded_len(usize::MAX).is_err());
    }

    #[test]
    fn encoded_and_final_budgets_fail_before_allocation() {
        let body = serde_json::json!({"messages": []});
        let limits = ImageLimits {
            per_image_decoded: 16,
            aggregate_decoded: 32,
            aggregate_encoded: 10,
            final_request: 128,
        };
        let mut budget = ImagePrefetchBudget::with_limits(&body, limits).unwrap();
        let error = encode_image(
            DownloadedImage {
                bytes: b"BM1234".to_vec(),
                mime_type: "image/bmp".into(),
            },
            &mut budget,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ImagePrefetchError::AggregateEncodedTooLarge { .. }
        ));

        let limits = ImageLimits {
            aggregate_encoded: 128,
            final_request: serde_json::to_vec(&body).unwrap().len() + 10,
            ..limits
        };
        let mut budget = ImagePrefetchBudget::with_limits(&body, limits).unwrap();
        let error = encode_image(
            DownloadedImage {
                bytes: b"BM1234".to_vec(),
                mime_type: "image/bmp".into(),
            },
            &mut budget,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ImagePrefetchError::FinalRequestTooLarge { .. }
        ));
    }

    #[test]
    fn claude_payload_reuses_data_url_allocation() {
        let body = serde_json::json!({"messages": []});
        let mut budget = ImagePrefetchBudget::with_limits(
            &body,
            ImageLimits {
                per_image_decoded: 64,
                aggregate_decoded: 64,
                aggregate_encoded: 128,
                final_request: 1024,
            },
        )
        .unwrap();
        let fetched = encode_image(
            DownloadedImage {
                bytes: b"BMpayload".to_vec(),
                mime_type: "image/bmp".into(),
            },
            &mut budget,
        )
        .unwrap();
        assert!(fetched.data_url.starts_with("data:image/bmp;base64,"));
        let (mime, data) = fetched.into_claude_source();
        assert_eq!(mime, "image/bmp");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap(),
            b"BMpayload"
        );
    }
}
