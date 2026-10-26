FROM node:22-trixie-slim@sha256:93c4ef18bd9f668c013e8800677f6709fe426db3f29fe758d34dd72600018a7e

# Debian's slim image has no CA bundle. Bootstrap it from the signed Debian
# archive before using HTTPS for the fixed snapshot and Chrome download.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates=20250419 \
    && rm -rf /var/lib/apt/lists/*

# Snapshot fixes the browser's libraries, fonts and ImageMagick as well as the
# explicitly pinned Node and Chrome downloads. Chrome for Testing is amd64.
RUN printf '%s\n' \
    'deb [check-valid-until=no] https://snapshot.debian.org/archive/debian/20261025T000000Z trixie main' \
    'deb [check-valid-until=no] https://snapshot.debian.org/archive/debian-security/20261025T000000Z trixie-security main' \
    > /etc/apt/sources.list \
    && rm -f /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
       ca-certificates curl unzip openssl libnss3-tools \
       fonts-liberation fonts-noto-core \
       imagemagick=8:7.1.1.43+dfsg1-1+deb13u12 \
       libasound2t64 libatk-bridge2.0-0t64 libatk1.0-0t64 \
       libatspi2.0-0t64 libcairo2 libcups2t64 libdbus-1-3 libdrm2 libgbm1 \
       libglib2.0-0t64 libnspr4 libnss3 libpango-1.0-0 libpangocairo-1.0-0 \
       libx11-6 libxcb1 libxcomposite1 \
       libxdamage1 libxext6 libxfixes3 libxkbcommon0 libxrandr2 \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://storage.googleapis.com/chrome-for-testing-public/155.0.8059.79/linux64/chrome-linux64.zip -o /tmp/chrome.zip \
    && echo 'bcd9a79130d7e44d70f93be87042e9bcee7fe0f0b3898e06b8c9bed5fb619dcd  /tmp/chrome.zip' | sha256sum -c - \
    && unzip -q /tmp/chrome.zip -d /opt \
    && rm /tmp/chrome.zip \
    && mv /opt/chrome-linux64/chrome_sandbox /opt/chrome-linux64/chrome-sandbox \
    && chown root:root /opt/chrome-linux64/chrome-sandbox \
    && chmod 0555 /opt/chrome-linux64/chrome-sandbox

WORKDIR /opt/ui-browser
COPY package.json package-lock.json ./
ENV PUPPETEER_SKIP_DOWNLOAD=true PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
RUN npm ci --omit=dev --ignore-scripts --no-audit --no-fund

# These are all distinct WOFF2 subset URLs returned for the dashboard's exact
# Google Fonts import and the pinned Chrome 155 user agent. Download once at
# build time, never from the running (internal-only) test network. fonts.css
# retains Google's ordered face descriptors and maps URLs to these local names.
RUN mkdir -p fonts \
    && curl -fsSL https://fonts.gstatic.com/s/inter/v20/UcC73FwrK3iLTeHuS_nVMrMxCp50SjIa1ZL7.woff2 -o fonts/inter.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/inter/v20/UcC73FwrK3iLTeHuS_nVMrMxCp50SjIa2JL7SUc.woff2 -o fonts/inter-cyrillic-ext.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/inter/v20/UcC73FwrK3iLTeHuS_nVMrMxCp50SjIa0ZL7SUc.woff2 -o fonts/inter-cyrillic.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/inter/v20/UcC73FwrK3iLTeHuS_nVMrMxCp50SjIa2ZL7SUc.woff2 -o fonts/inter-greek-ext.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/inter/v20/UcC73FwrK3iLTeHuS_nVMrMxCp50SjIa1pL7SUc.woff2 -o fonts/inter-greek.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/inter/v20/UcC73FwrK3iLTeHuS_nVMrMxCp50SjIa2pL7SUc.woff2 -o fonts/inter-vietnamese.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/inter/v20/UcC73FwrK3iLTeHuS_nVMrMxCp50SjIa25L7SUc.woff2 -o fonts/inter-latin-ext.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/instrumentserif/v5/jizBRFtNs2ka5fXjeivQ4LroWlx-6zUTjg.woff2 -o fonts/instrument-serif.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/instrumentserif/v5/jizBRFtNs2ka5fXjeivQ4LroWlx-6zsTjmbI.woff2 -o fonts/instrument-serif-latin-ext.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/instrumentserif/v5/jizHRFtNs2ka5fXjeivQ4LroWlx-6zAjjH7M.woff2 -o fonts/instrument-serif-italic.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/instrumentserif/v5/jizHRFtNs2ka5fXjeivQ4LroWlx-6zAjgn7MsNo.woff2 -o fonts/instrument-serif-italic-latin-ext.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/jetbrainsmono/v24/tDbv2o-flEEny0FZhsfKu5WU4zr3E_BX0PnT8RD8yKwBNntkaToggR7BYRbKPxDcwg.woff2 -o fonts/jetbrains-mono.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/jetbrainsmono/v24/tDbv2o-flEEny0FZhsfKu5WU4zr3E_BX0PnT8RD8yKwBNntkaToggR7BYRbKPx3cwhsk.woff2 -o fonts/jetbrains-mono-cyrillic-ext.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/jetbrainsmono/v24/tDbv2o-flEEny0FZhsfKu5WU4zr3E_BX0PnT8RD8yKwBNntkaToggR7BYRbKPxTcwhsk.woff2 -o fonts/jetbrains-mono-cyrillic.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/jetbrainsmono/v24/tDbv2o-flEEny0FZhsfKu5WU4zr3E_BX0PnT8RD8yKwBNntkaToggR7BYRbKPxPcwhsk.woff2 -o fonts/jetbrains-mono-greek.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/jetbrainsmono/v24/tDbv2o-flEEny0FZhsfKu5WU4zr3E_BX0PnT8RD8yKwBNntkaToggR7BYRbKPx_cwhsk.woff2 -o fonts/jetbrains-mono-vietnamese.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/jetbrainsmono/v24/tDbv2o-flEEny0FZhsfKu5WU4zr3E_BX0PnT8RD8yKwBNntkaToggR7BYRbKPx7cwhsk.woff2 -o fonts/jetbrains-mono-latin-ext.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/jetbrainsmono/v24/tDba2o-flEEny0FZhsfKu5WU4xD-IQ-PuZJJXxfpAO-LfmOXmaI.woff2 -o fonts/jetbrains-mono-italic.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/jetbrainsmono/v24/tDba2o-flEEny0FZhsfKu5WU4xD-IQ-PuZJJXxfpAO-LfmOamaKjhw.woff2 -o fonts/jetbrains-mono-italic-cyrillic-ext.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/jetbrainsmono/v24/tDba2o-flEEny0FZhsfKu5WU4xD-IQ-PuZJJXxfpAO-LfmOTmaKjhw.woff2 -o fonts/jetbrains-mono-italic-cyrillic.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/jetbrainsmono/v24/tDba2o-flEEny0FZhsfKu5WU4xD-IQ-PuZJJXxfpAO-LfmOUmaKjhw.woff2 -o fonts/jetbrains-mono-italic-greek.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/jetbrainsmono/v24/tDba2o-flEEny0FZhsfKu5WU4xD-IQ-PuZJJXxfpAO-LfmOYmaKjhw.woff2 -o fonts/jetbrains-mono-italic-vietnamese.woff2 \
    && curl -fsSL https://fonts.gstatic.com/s/jetbrainsmono/v24/tDba2o-flEEny0FZhsfKu5WU4xD-IQ-PuZJJXxfpAO-LfmOZmaKjhw.woff2 -o fonts/jetbrains-mono-italic-latin-ext.woff2 \
    && printf '%s\n' \
      '3100e775e8616cd2611beecfa23a4263d7037586789b43f035236a2e6fbd4c62  fonts/inter.woff2' \
      'ca157063339ac4ad418f214f3abfed119b0798ab4d377386ce5c9e5a7a435ebd  fonts/inter-cyrillic-ext.woff2' \
      '71d5ee93cc1e9f1d520a3a8b66456de18c7879d8df09d57fcd2eaff75fef0075  fonts/inter-cyrillic.woff2' \
      '6e9e020a25f9b56d418f2c085b1d3c09725a4da23fe693a5b463064606732190  fonts/inter-greek-ext.woff2' \
      '1be3448e292fbf05ffe176fe1e43f135013d50b1e7d324ad1a558f623d3bb6f6  fonts/inter-greek.woff2' \
      '5c66f9e07e90c6d4ac4922cc68d60de26c17b1858e677fb5e603fce3952b3ff2  fonts/inter-vietnamese.woff2' \
      '34b9c504cab7a73e37b746343a449132e56cf7b5481af2cb81dc74dcff25c956  fonts/inter-latin-ext.woff2' \
      '5eb09b5ac0e28b67c2f041c8ba6d244604ca0c0980d65912ab2d47fed84ddc31  fonts/instrument-serif.woff2' \
      '290e6267dd833bf5f899eba4c29ad0a9b09dbe53f6075b18af38057159e1ff20  fonts/instrument-serif-latin-ext.woff2' \
      '5a51946dfffa82972bc98745359c46761515641fda557c25116459a9f83da4a7  fonts/instrument-serif-italic.woff2' \
      'd0014821252a8a770bf7551641572f239a8ede83d75652d84fef0c3627751baf  fonts/instrument-serif-italic-latin-ext.woff2' \
      '83c005d49d8a6a50474c73a5a36ac0468076e9c4a29da7bdb14995d80560a5be  fonts/jetbrains-mono.woff2' \
      '62213be8a78b42f1e29d1452d91e2f8b3e745572a9dd98d3941e39fa00b37d76  fonts/jetbrains-mono-cyrillic-ext.woff2' \
      'e17cfd15fb96909d64095015f958207063a0c07191da3512df7d560a781aebdf  fonts/jetbrains-mono-cyrillic.woff2' \
      '0a557721b1f8b36d3f3f84442689a71ca4a744300abcb46a1953f51bfc663b66  fonts/jetbrains-mono-greek.woff2' \
      'c89b9cc0bc6262bd4f8d8494b6961601f3aefa829d08c2e3635f4d501d3a47c2  fonts/jetbrains-mono-vietnamese.woff2' \
      'db5ff4db83e580426280e9337a58dc57d3a83784a1b03ad80914651594441d52  fonts/jetbrains-mono-latin-ext.woff2' \
      '87ddac4a62229787528cf4ac3fa58137b62a9aec364c44fc0f40470bcda9efba  fonts/jetbrains-mono-italic.woff2' \
      '4689f72f63b93f1d70c7e22f6c0d76e3800697c13be813a27e2693b36b575da9  fonts/jetbrains-mono-italic-cyrillic-ext.woff2' \
      'f1c1fd2a2efbeafa8b0bd8afc5729f3b2c3ff6f936a08d5d039388a9995bc59b  fonts/jetbrains-mono-italic-cyrillic.woff2' \
      '61c8c6f188a5f4af2f18edb581f075e10b7284888465eec4bc057114123bfcc1  fonts/jetbrains-mono-italic-greek.woff2' \
      'a2526a850fe7e698184c63ff9cbd28c56d8f18b4946fda1a1834a66db0da4c95  fonts/jetbrains-mono-italic-vietnamese.woff2' \
      'a53b6b29b1621cdf51140761869e160521fc566a4a9f312d9b92e7178d433bc7  fonts/jetbrains-mono-italic-latin-ext.woff2' \
      | sha256sum -c -

# Generate a disposable, name-constrained CA and an app-only leaf. The CA key
# is discarded in the same layer; the remaining leaf key is test data only.
RUN mkdir -p tls \
    && openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
      -keyout tls/ca.key -out tls/ca.crt -subj '/CN=OpenProxy UI fixture CA' \
      -addext 'basicConstraints=critical,CA:TRUE,pathlen:0' \
      -addext 'keyUsage=critical,keyCertSign,cRLSign' \
      -addext 'nameConstraints=critical,permitted;DNS:app' \
    && openssl req -newkey rsa:2048 -nodes -keyout tls/app.key -out tls/app.csr -subj '/CN=app' \
    && printf '%s\n' 'basicConstraints=critical,CA:FALSE' 'keyUsage=critical,digitalSignature,keyEncipherment' \
      'extendedKeyUsage=serverAuth' 'subjectAltName=DNS:app' > tls/extensions.cnf \
    && openssl x509 -req -sha256 -days 3650 -in tls/app.csr -CA tls/ca.crt -CAkey tls/ca.key \
      -CAcreateserial -out tls/app.crt -extfile tls/extensions.cnf \
    && openssl verify -CAfile tls/ca.crt tls/app.crt \
    && rm tls/ca.key tls/ca.srl tls/app.csr tls/extensions.cnf \
    && chmod 0444 tls/app.key

COPY entrypoint.sh proxy.mjs fonts.css ./
RUN chmod 0555 entrypoint.sh \
    && curl -fsSL https://raw.githubusercontent.com/google/fonts/b0928647ba26c8d55548aa19feb2f7aeba622d4a/ofl/inter/OFL.txt -o fonts/inter.OFL.txt \
    && curl -fsSL https://raw.githubusercontent.com/google/fonts/5e0122a40050ed2cc20e9d5c387d19dd13c6c69f/ofl/instrumentserif/OFL.txt -o fonts/instrument-serif.OFL.txt \
    && curl -fsSL https://raw.githubusercontent.com/google/fonts/2e05c1cf00a6e4f40a4b931600a90881c26e15cd/ofl/jetbrainsmono/OFL.txt -o fonts/jetbrains-mono.OFL.txt \
    && printf '%s\n' \
      '5b9321a4298cfeb6b34354164a1c3afc3db114569984c502b9b35d988fd58c57  fonts/inter.OFL.txt' \
      '129ed7618959716959f2941fdd5b49e0ad6e6c1d78726761786a00253d865521  fonts/instrument-serif.OFL.txt' \
      'b2fe5e8987594e9ffd1d2ca52a2f5d73eb8335243893c5d6254b5ad69269591d  fonts/jetbrains-mono.OFL.txt' \
      | sha256sum -c -

USER 1000:1000
