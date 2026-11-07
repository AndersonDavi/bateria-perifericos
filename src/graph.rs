//! "Ver gráfico": writes a self-contained HTML page (data embedded, chart drawn
//! in SVG by a few lines of JS, no external libraries) and opens it in the browser.
//! Nothing stays in memory: the page is generated only when asked for.

use crate::stats::{History, Session};
use std::path::PathBuf;

pub struct Summary<'a> {
    pub name: &'a str,
    pub now_text: String,
    pub life: String,
    pub cycles: String,
    pub health: String,
}

fn sessions_json(v: &[Session]) -> String {
    let items: Vec<String> = v
        .iter()
        .map(|s| format!("[{},{},{},{},{}]", s.t0, s.t1, s.p0, s.p1, s.done as u8))
        .collect();
    format!("[{}]", items.join(","))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Writes the page next to the history and returns its path.
pub fn write(h: &History, s: &Summary) -> Option<PathBuf> {
    let path = h.path()?.with_file_name("grafico.html");
    let html = TEMPLATE
        .replace("{{NAME}}", &esc(s.name))
        .replace("{{NOW}}", &esc(&s.now_text))
        .replace("{{LIFE}}", &esc(&s.life))
        .replace("{{CYCLES}}", &esc(&s.cycles))
        .replace("{{HEALTH}}", &esc(&s.health))
        .replace("{{ROWS}}", &h.rows_json())
        .replace("{{DIS}}", &sessions_json(&h.discharges()))
        .replace("{{CHG}}", &sessions_json(&h.charges()))
        .replace("{{NOWTS}}", &crate::stats::now().to_string());
    std::fs::write(&path, html).ok()?;
    Some(path)
}

const TEMPLATE: &str = r##"<!doctype html>
<html lang="es"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Batería: {{NAME}}</title>
<style>
:root{--bg:#f6f7f9;--card:#fff;--fg:#111827;--mute:#6b7280;--line:#e5e7eb;--grid:#eef0f3;--g:#16a34a;--a:#d97706;--r:#dc2626;--chg:rgba(37,99,235,.10)}
@media (prefers-color-scheme:dark){:root{--bg:#0f1417;--card:#162026;--fg:#e5e7eb;--mute:#9ca3af;--line:#26323a;--grid:#1d2930;--g:#22c55e;--a:#f59e0b;--r:#ef4444;--chg:rgba(96,165,250,.13)}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.45 system-ui,"Segoe UI",sans-serif}
main{max-width:1000px;margin:0 auto;padding:24px 16px 48px}
h1{font-size:20px;margin:0 0 2px}.sub{color:var(--mute);margin:0 0 18px}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr));gap:12px;margin-bottom:16px}
.card{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:12px 14px}
.card b{display:block;font-size:12px;color:var(--mute);font-weight:500;margin-bottom:4px}
.box{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:14px;margin-bottom:16px}
.bar{display:flex;gap:6px;align-items:center;margin-bottom:8px;flex-wrap:wrap}
.bar h2{font-size:15px;margin:0 auto 0 0}
button{background:transparent;color:var(--fg);border:1px solid var(--line);border-radius:8px;padding:4px 10px;cursor:pointer;font:inherit}
button.on{background:var(--fg);color:var(--card)}
svg{display:block;width:100%;height:auto;overflow:visible}
.ax{fill:var(--mute);font-size:11px}
#tip{position:fixed;pointer-events:none;background:var(--card);border:1px solid var(--line);border-radius:8px;padding:6px 9px;font-size:12px;display:none;box-shadow:0 4px 14px rgba(0,0,0,.15)}
.legend{color:var(--mute);font-size:12px;margin-top:6px}.legend i{display:inline-block;width:10px;height:10px;border-radius:2px;margin:0 4px 0 10px;vertical-align:-1px}
table{width:100%;border-collapse:collapse;font-size:13px}th,td{text-align:left;padding:6px 8px;border-bottom:1px solid var(--line)}th{color:var(--mute);font-weight:500}
.cols{display:grid;grid-template-columns:repeat(auto-fit,minmax(300px,1fr));gap:16px}
.empty{color:var(--mute);padding:8px 0}
</style></head><body><main>
<h1>{{NAME}}</h1><p class="sub">{{NOW}}</p>
<div class="cards">
<div class="card"><b>Duración por carga</b>{{LIFE}}</div>
<div class="card"><b>Ciclos</b>{{CYCLES}}</div>
<div class="card"><b>Salud de la batería</b>{{HEALTH}}</div>
</div>
<div class="box">
<div class="bar"><h2>Nivel de batería</h2>
<button data-d="1">24 h</button><button data-d="7" class="on">7 días</button><button data-d="30">30 días</button><button data-d="0">Todo</button></div>
<svg id="c" viewBox="0 0 960 300"></svg>
<div class="legend"><i style="background:var(--chg)"></i>enchufado<i style="background:var(--g)"></i>&gt; 40 %<i style="background:var(--a)"></i>16–40 %<i style="background:var(--r)"></i>≤ 15 %</div>
</div>
<div class="cols">
<div class="box"><h2 style="font-size:15px;margin:0 0 8px">Cargas</h2><div id="tc"></div></div>
<div class="box"><h2 style="font-size:15px;margin:0 0 8px">Descargas</h2><div id="td"></div></div>
</div>
<div id="tip"></div>
</main>
<script>
const R={{ROWS}},DIS={{DIS}},CHG={{CHG}},NOW={{NOWTS}};
const css=n=>getComputedStyle(document.documentElement).getPropertyValue(n).trim();
const col=p=>p<=15?css('--r'):p<=40?css('--a'):css('--g');
const fmt=t=>new Date(t*1000).toLocaleString('es-CO',{day:'numeric',month:'short',hour:'2-digit',minute:'2-digit'});
const dur=s=>{const m=Math.round(s/60),d=Math.floor(m/1440),h=Math.floor(m/60)%24,mm=m%60;return d>=2?d+' días '+h+' h':d==1?'1 día '+h+' h':h?h+' h '+mm+' min':mm+' min'};
const W=960,H=300,L=36,T=10,B=26,Rr=8;
function draw(days){
 const svg=document.getElementById('c');
 if(!R.length){svg.innerHTML='<text x="20" y="40" class="ax">Todavía no hay datos.</text>';return}
 const t1=NOW,t0=days?t1-days*86400:Math.min(R[0][0],t1-3600);
 const X=t=>L+(Math.max(t,t0)-t0)/(t1-t0)*(W-L-Rr),Y=p=>T+(100-p)/100*(H-T-B);
 let s='';
 for(const p of [0,25,50,75,100])s+=`<line x1="${L}" x2="${W-Rr}" y1="${Y(p)}" y2="${Y(p)}" stroke="${css('--grid')}"/><text class="ax" x="${L-6}" y="${Y(p)+4}" text-anchor="end">${p}</text>`;
 for(const c of CHG){if(c[1]<t0)continue;s+=`<rect x="${X(c[0])}" y="${T}" width="${Math.max(2,X(c[1])-X(c[0]))}" height="${H-T-B}" fill="${css('--chg')}"/>`}
 const pts=[];let prev=null;
 for(const r of R){if(r[0]<t0){prev=r;continue}if(prev&&!pts.length)pts.push([t0,prev[1]]);if(pts.length)pts.push([r[0],pts[pts.length-1][1]]);pts.push([r[0],r[1]])}
 if(!pts.length&&prev)pts.push([t0,prev[1]]);
 if(pts.length)pts.push([t1,pts[pts.length-1][1]]);
 for(let i=1;i<pts.length;i++){const a=pts[i-1],b=pts[i];s+=`<line x1="${X(a[0])}" y1="${Y(a[1])}" x2="${X(b[0])}" y2="${Y(b[1])}" stroke="${col(Math.min(a[1],b[1]))}" stroke-width="2.4" stroke-linecap="round"/>`}
 const n=days==1?6:days==7?7:6;for(let i=0;i<=n;i++){const t=t0+(t1-t0)*i/n;s+=`<text class="ax" x="${X(t)}" y="${H-6}" text-anchor="${i==0?'start':i==n?'end':'middle'}">${new Date(t*1000).toLocaleString('es-CO',days==1?{hour:'2-digit',minute:'2-digit'}:{day:'numeric',month:'short'})}</text>`}
 s+=`<line id="hv" y1="${T}" y2="${H-B}" stroke="${css('--mute')}" stroke-dasharray="3 3" visibility="hidden"/><rect x="${L}" y="${T}" width="${W-L-Rr}" height="${H-T-B}" fill="transparent" id="hit"/>`;
 svg.innerHTML=s;
 const tip=document.getElementById('tip'),hv=document.getElementById('hv');
 document.getElementById('hit').onmousemove=e=>{const bb=svg.getBoundingClientRect(),x=(e.clientX-bb.left)*W/bb.width,t=t0+(x-L)/(W-L-Rr)*(t1-t0);let v=null;for(const p of pts){if(p[0]<=t)v=p[1]}if(v==null){tip.style.display='none';return}
  const ch=CHG.some(c=>t>=c[0]&&t<=c[1]);hv.setAttribute('x1',x);hv.setAttribute('x2',x);hv.setAttribute('visibility','visible');
  tip.innerHTML=`<b>${v} %</b>${ch?' · enchufado':''}<br>${fmt(t)}`;tip.style.display='block';tip.style.left=(e.clientX+12)+'px';tip.style.top=(e.clientY-10)+'px'};
 document.getElementById('hit').onmouseleave=()=>{tip.style.display='none';hv.setAttribute('visibility','hidden')};
}
function table(id,rows,head,fn){const el=document.getElementById(id);if(!rows.length){el.innerHTML='<div class="empty">Sin datos todavía.</div>';return}
 el.innerHTML='<table><tr>'+head.map(h=>'<th>'+h+'</th>').join('')+'</tr>'+rows.slice(-12).reverse().map(r=>'<tr>'+fn(r).map(c=>'<td>'+c+'</td>').join('')+'</tr>').join('')+'</table>'}
table('tc',CHG,['Inicio','Nivel','Tardó'],c=>[fmt(c[0]),c[2]+' % → '+c[3]+' %',c[4]?dur(c[1]-c[0]):'en curso']);
table('td',DIS,['Inicio','Nivel','Duró','Ritmo'],d=>{const h=(d[1]-d[0])/3600,dp=d[2]-d[3];return[fmt(d[0]),d[2]+' % → '+d[3]+' %',d[4]?dur(d[1]-d[0]):'en curso',dp>0&&h>0?(dp/h*24).toFixed(1).replace('.',',')+' %/día':'—']});
let cur=7;document.querySelectorAll('button').forEach(b=>b.onclick=()=>{document.querySelectorAll('button').forEach(x=>x.classList.remove('on'));b.classList.add('on');cur=+b.dataset.d;draw(cur)});
draw(cur);matchMedia('(prefers-color-scheme:dark)').addEventListener('change',()=>draw(cur));
</script></body></html>
"##;
