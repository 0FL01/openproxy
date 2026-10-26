import http from 'node:http';

// Synthetic state only; it disappears when the backend container stops.
let clientStream;
let aborted = false;

http.createServer((req, res) => {
  if (req.url === '/_fixture/state') {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ clientStream, aborted }));
    return;
  }

  if (req.url !== '/api/translator/console-logs/stream' && req.url !== '/_fixture/abort') {
    res.writeHead(404).end();
    return;
  }

  if (req.url === '/api/translator/console-logs/stream') {
    const stream = clientStream = { closed: false };
    res.on('close', () => { stream.closed = !res.writableFinished; });
  }
  res.writeHead(200, { 'content-type': 'text/event-stream' });
  res.write('data: ready\n\n');
  if (req.url === '/_fixture/abort') {
    aborted = false;
    setTimeout(() => {
      aborted = true;
      res.destroy();
    }, 500);
  }
}).listen(4623, '0.0.0.0');
