// Local static test server; no packages or imported scripts/ required.
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname,sep} from 'node:path';
const root = resolve(import.meta.dirname,'storybook-static');
const port = Number(process.env.STORYBOOK_PORT ?? 6006);
const types = {'.html':'text/html','.js':'text/javascript','.css':'text/css','.json':'application/json','.svg':'image/svg+xml','.png':'image/png','.woff2':'font/woff2'};
createServer(async(req,res)=>{
  try {
    const pathname = decodeURIComponent(new URL(req.url,'http://localhost').pathname);
    const file = resolve(root,'.'+(pathname==='/'?'/index.html':pathname));
    if (!file.startsWith(root+sep)) {res.writeHead(403).end();return;}
    const bytes = await readFile(file);res.writeHead(200,{'Content-Type':types[extname(file)]??'application/octet-stream'}).end(bytes);
  } catch {res.writeHead(404).end();}
}).listen(port,'127.0.0.1');
