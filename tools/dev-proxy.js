// One-origin reverse proxy for the local Pimble stack (stands in for jkbase's edge):
//   /api/*  -> the accounts service on 8080 (plain HTTP)
//   /rpc    -> the hosted Pimble server on 7463 (HTTP + WebSocket upgrade)
const http = require('http');
const net = require('net');

const LISTEN = parseInt(process.env.PROXY_PORT || '8090', 10);
const API = { host: '127.0.0.1', port: 8080 };
const RPC = { host: '127.0.0.1', port: parseInt(process.env.RPC_PORT || '7463', 10) };

function target(url) {
  if (url.startsWith('/api/')) return API;
  if (url === '/rpc' || url.startsWith('/rpc?')) return RPC;
  return null;
}

const server = http.createServer((req, res) => {
  const t = target(req.url);
  if (!t) { res.writeHead(404); res.end('no route'); return; }
  // The hosted server serves the RPC at "/", the proxy exposes it at /rpc.
  const path = t === RPC ? req.url.replace(/^\/rpc/, '/') : req.url;
  const headers = { ...req.headers, host: `${t.host}:${t.port}` };
  const up = http.request({ host: t.host, port: t.port, method: req.method, path, headers }, (ur) => {
    res.writeHead(ur.statusCode, ur.headers);
    ur.pipe(res);
  });
  up.on('error', (e) => { res.writeHead(502); res.end(String(e)); });
  req.pipe(up);
});

server.on('upgrade', (req, socket, head) => {
  const t = target(req.url);
  if (!t) { socket.destroy(); return; }
  const path = t === RPC ? req.url.replace(/^\/rpc/, '/') : req.url;
  const up = net.connect(t.port, t.host, () => {
    let raw = `${req.method} ${path} HTTP/1.1\r\n`;
    for (let i = 0; i < req.rawHeaders.length; i += 2) {
      const k = req.rawHeaders[i];
      const v = k.toLowerCase() === 'host' ? `${t.host}:${t.port}` : req.rawHeaders[i + 1];
      raw += `${k}: ${v}\r\n`;
    }
    raw += '\r\n';
    up.write(raw);
    if (head && head.length) up.write(head);
    socket.pipe(up);
    up.pipe(socket);
  });
  up.on('error', () => socket.destroy());
  socket.on('error', () => up.destroy());
});

server.listen(LISTEN, '127.0.0.1', () => console.log(`proxy on http://127.0.0.1:${LISTEN} -> api:${API.port} rpc:${RPC.port}`));
