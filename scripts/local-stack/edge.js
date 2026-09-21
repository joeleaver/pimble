// One origin in front of everything, as the platform edge is in production:
// /app/* static from web/dist, /rpc (WebSocket) to the hosted Pimble server,
// the rest to the accounts service.
const http = require('http'), net = require('net'), fs = require('fs'), path = require('path');
const [listen, cloud, rpc, dist] = [+process.argv[2], +process.argv[3], +process.argv[4], process.argv[5]];
// A second instance on another loopback address (127.0.0.2) gives a second cookie jar
// in one browser: a second signed-in account side by side.
const host = process.argv[6] || '127.0.0.1';
const types = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.ico': 'image/x-icon', '.json': 'application/json' };
const server = http.createServer((req, res) => {
  const url = req.url.split('?')[0];
  if (url === '/app' || url.startsWith('/app/')) {
    let file = path.join(dist, url.slice(4) || '/');
    if (!file.startsWith(dist) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) file = path.join(dist, 'index.html');
    res.writeHead(200, { 'content-type': types[path.extname(file)] || 'application/octet-stream', 'cache-control': 'no-store' });
    return fs.createReadStream(file).pipe(res);
  }
  if (url === '/' || url === '/login.html') { res.writeHead(302, { location: '/app/login' }); return res.end(); }
  const up = http.request({ host: '127.0.0.1', port: cloud, path: req.url, method: req.method, headers: req.headers }, (r) => {
    res.writeHead(r.statusCode, r.headers); r.pipe(res);
  });
  up.on('error', () => { res.writeHead(502); res.end('accounts service down'); });
  req.pipe(up);
});
server.on('upgrade', (req, socket, head) => {
  const up = net.connect(rpc, '127.0.0.1', () => {
    up.write(`${req.method} ${req.url} HTTP/1.1\r\n` + Object.entries(req.headers).map(([k, v]) => `${k}: ${v}`).join('\r\n') + '\r\n\r\n');
    if (head.length) up.write(head);
    socket.pipe(up); up.pipe(socket);
  });
  up.on('error', () => socket.destroy()); socket.on('error', () => up.destroy());
});
server.listen(listen, host);
