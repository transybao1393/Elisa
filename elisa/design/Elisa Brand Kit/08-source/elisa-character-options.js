(function () {
  const n = v => Math.round(v * 100) / 100;
  const rr = (x, y, w, h, r) => { r = Math.min(r, w / 2, h / 2); return `M${n(x + r)} ${n(y)}H${n(x + w - r)}A${r} ${r} 0 0 1 ${n(x + w)} ${n(y + r)}V${n(y + h - r)}A${r} ${r} 0 0 1 ${n(x + w - r)} ${n(y + h)}H${n(x + r)}A${r} ${r} 0 0 1 ${n(x)} ${n(y + h - r)}V${n(y + r)}A${r} ${r} 0 0 1 ${n(x + r)} ${n(y)}Z`; };
  const K = { coral: '#D77757', cyan: '#82C8DC', green: '#78C878', amber: '#E6BE50', ink: '#1E1E24', white: '#E8E8E8' };
  const PAL = {
    graphite: { body: '#34343F', rim: '#50505F', light: '#474755', eye: K.white },
    slate: { body: '#454552', rim: '#62627274', light: '#5A5A6A', eye: K.white },
    deep: { body: '#26262E', rim: '#46465A', light: '#34343F', eye: K.white },
    steel: { body: '#36404E', rim: '#52607A', light: '#465264', eye: K.white },
    ivory: { body: '#E4E4E8', rim: '#BDBDC6', light: '#F7F7F9', eye: K.ink }
  };
  PAL.slate.rim = '#626272';

  const B = {
    dome: { d: 'M44 150C44 98 70 68 100 68C130 68 156 98 156 150C156 164 148 172 134 172H66C52 172 44 164 44 150Z', top: 68, eyeY: 112, ex: 16, hw: 50, accY: 138, accW: 44, earX: 24, earY: 80 },
    round: { d: 'M46 118a54 54 0 1 0 108 0a54 54 0 1 0-108 0Z', top: 64, eyeY: 108, ex: 17, hw: 52, accY: 142, accW: 40, earX: 28, earY: 76 },
    pear: { d: 'M70 100C70 76 84 64 100 64C116 64 130 76 130 100C130 120 158 134 158 156C158 166 150 172 140 172H60C50 172 42 166 42 156C42 134 70 120 70 100Z', top: 64, eyeY: 92, ex: 13, hw: 30, accY: 118, accW: 26, earX: 18, earY: 74 },
    drop: { d: 'M100 52C112 74 156 104 156 136C156 158 132 172 100 172C68 172 44 158 44 136C44 104 88 74 100 52Z', top: 56, eyeY: 120, ex: 16, hw: 48, accY: 148, accW: 42, earX: 18, earY: 80 },
    square: { d: rr(48, 72, 104, 100, 22), top: 72, eyeY: 112, ex: 18, hw: 52, accY: 152, accW: 40, earX: 30, earY: 78 },
    bean: { d: 'M36 140C36 100 64 84 100 84C136 84 164 100 164 140C164 162 150 172 130 172H70C50 172 36 162 36 140Z', top: 84, eyeY: 120, ex: 17, hw: 60, accY: 150, accW: 52, earX: 32, earY: 94 },
    tall: { d: rr(64, 52, 72, 120, 36), top: 52, eyeY: 92, ex: 13, hw: 36, accY: 124, accW: 30, earX: 18, earY: 62 },
    egg: { d: 'M100 60C134 60 154 100 154 132C154 158 130 172 100 172C70 172 46 158 46 132C46 100 66 60 100 60Z', top: 60, eyeY: 110, ex: 16, hw: 50, accY: 142, accW: 44, earX: 22, earY: 72 },
    ghost: { d: 'M46 110C46 82 70 64 100 64C130 64 154 82 154 110V164Q145 176 136 164Q127 152 118 164Q109 176 100 164Q91 152 82 164Q73 176 64 164Q55 152 46 164Z', top: 64, eyeY: 106, ex: 16, hw: 54, accY: 136, accW: 46, earX: 24, earY: 76, noFeet: 1 },
    mushroom: { d: rr(64, 104, 72, 68, 26), cap: 'M36 104C36 70 66 50 100 50C134 50 164 70 164 104C164 112 156 114 148 112H52C44 114 36 112 36 104Z', top: 50, eyeY: 136, ex: 12, hw: 36, accY: 160, accW: 26, earX: 0, earY: 56 },
    wide: { d: rr(40, 96, 120, 76, 38), top: 96, eyeY: 128, ex: 18, hw: 60, accY: 158, accW: 46, earX: 34, earY: 104 },
    owl: { d: 'M48 150C48 100 56 72 100 72C144 72 152 100 152 150C152 164 144 172 130 172H70C56 172 48 164 48 150Z', top: 72, eyeY: 108, ex: 21, hw: 52, accY: 146, accW: 44, earX: 34, earY: 80 },
    acorn: { d: 'M52 124a48 48 0 1 0 96 0a48 48 0 1 0-96 0Z', cap: 'M48 112C48 82 72 66 100 66C128 66 152 82 152 112C152 116 148 118 144 117H56C52 118 48 116 48 112Z', top: 58, eyeY: 132, ex: 15, hw: 46, accY: 154, accW: 36, earX: 0, earY: 66 },
    block: { d: rr(66, 60, 68, 112, 12), top: 60, eyeY: 96, ex: 13, hw: 34, accY: 160, accW: 26, earX: 20, earY: 66 },
    stack: { d: rr(52, 132, 96, 40, 18), mid: rr(58, 100, 84, 38, 17), head: rr(62, 64, 76, 42, 18), top: 64, eyeY: 85, ex: 14, hw: 38, accY: 0, accW: 0, earX: 24, earY: 70 },
    keycap: { d: rr(44, 76, 112, 96, 20), face: rr(56, 82, 88, 70, 14), top: 76, eyeY: 112, ex: 16, hw: 44, accY: 162, accW: 36, earX: 30, earY: 82 },
    e: { ring: 'M140 118A40 40 0 1 0 128 148', top: 60, eyeY: 80, ex: 15, hw: 56, accY: 0, accW: 0, earX: 26, earY: 72 }
  };

  const CAT = 'M-13 6C-16 -14 -12 -30 -6 -32C0 -33 8 -20 13 2Z', CAT_IN = 'M-8 2C-10 -11 -8 -21 -5 -23C-1 -23 3 -14 7 0Z';
  const FOX = 'M-14 8C-16 -20 -8 -42 -2 -44C4 -44 10 -20 14 4Z', FOX_IN = 'M-8 4C-9 -16 -5 -32 -2 -34C1 -34 5 -16 8 2Z';
  function ear(type, P) {
    const st = `stroke="${P.rim}" stroke-width="2" stroke-linejoin="round"`;
    switch (type) {
      case 'cat': return `<path d="${CAT}" fill="${P.body}" ${st}/><path d="${CAT_IN}" fill="${K.coral}" opacity=".85"/>`;
      case 'fox': return `<path d="${FOX}" fill="${P.body}" ${st}/><path d="${FOX_IN}" fill="${K.coral}" opacity=".85"/>`;
      case 'fennec': return `<g transform="rotate(-14) scale(1.45)"><path d="${CAT}" fill="${P.body}" ${st}/><path d="${CAT_IN}" fill="${K.coral}" opacity=".85"/></g>`;
      case 'lynx': return `<path d="${CAT}" fill="${P.body}" ${st}/><path d="${CAT_IN}" fill="${K.coral}" opacity=".85"/><path d="M-6 -32L-9 -44" stroke="${P.rim}" stroke-width="3" stroke-linecap="round"/>`;
      case 'round': return `<circle cy="-6" r="14" fill="${P.body}" ${st}/><circle cy="-6" r="7.5" fill="${K.coral}" opacity=".8"/>`;
      case 'biground': return `<circle cx="-4" cy="-10" r="20" fill="${P.body}" ${st}/><circle cx="-4" cy="-10" r="12" fill="${K.coral}" opacity=".75"/>`;
      case 'bunny': return `<g transform="rotate(-8)"><path d="${rr(-8, -58, 16, 64, 8)}" fill="${P.body}" ${st}/><path d="${rr(-4, -50, 8, 46, 4)}" fill="${K.coral}" opacity=".8"/></g>`;
      case 'tuft': return `<path d="M-12 8L-9 -22L10 4Z" fill="${P.body}" ${st}/>`;
      case 'horns': return `<path d="M-6 6C-9 -6 -5 -14 2 -16C1 -8 4 -2 7 3Z" fill="${P.light}" ${st}/>`;
      case 'moth': return `<path d="M0 4C-2 -14 -12 -26 -24 -26C-30 -26 -32 -20 -27 -17" fill="none" stroke="${P.rim}" stroke-width="3" stroke-linecap="round"/><path d="M-6 -12l-6 2M-12 -20l-5 4" stroke="${P.rim}" stroke-width="2" stroke-linecap="round"/>`;
      case 'antennae': return `<path d="M0 6L-6 -22" stroke="${P.rim}" stroke-width="3" stroke-linecap="round"/><circle cx="-6" cy="-24" r="5" fill="${K.coral}"/>`;
      case 'chevron': return `<path d="M-12 6L-4 -20L8 2" fill="none" stroke="${P.body}" stroke-width="11" stroke-linecap="round" stroke-linejoin="round"/><path d="M-4 -20" stroke="${K.coral}" stroke-width="6" stroke-linecap="round"/>`;
      case 'bolt': return `<circle r="6" fill="${P.light}" stroke="${P.rim}" stroke-width="2"/>`;
      default: return '';
    }
  }
  function topper(type, b, P) {
    const t = b.top;
    switch (type) {
      case 'antenna': return `<path d="M100 ${t + 4}V${t - 24}" stroke="${P.rim}" stroke-width="3.5" stroke-linecap="round"/><circle cx="100" cy="${t - 28}" r="6" fill="${K.coral}"/>`;
      case 'sprout': return `<path d="M100 ${t + 4}V${t - 14}" stroke="${K.green}" stroke-width="3.5" stroke-linecap="round"/><path d="M100 ${t - 12}C92 ${t - 26} 78 ${t - 26} 74 ${t - 20}C80 ${t - 10} 92 ${t - 8} 100 ${t - 12}Z" fill="${K.green}"/><path d="M100 ${t - 14}C108 ${t - 30} 122 ${t - 30} 126 ${t - 24}C120 ${t - 14} 108 ${t - 10} 100 ${t - 14}Z" fill="${K.green}" opacity=".8"/>`;
      case 'budcyan': return `<path d="M100 ${t + 4}V${t - 14}" stroke="${K.cyan}" stroke-width="3.5" stroke-linecap="round"/><path d="M100 ${t - 12}C92 ${t - 26} 78 ${t - 26} 74 ${t - 20}C80 ${t - 10} 92 ${t - 8} 100 ${t - 12}Z" fill="${K.cyan}"/><path d="M100 ${t - 14}C108 ${t - 30} 122 ${t - 30} 126 ${t - 24}C120 ${t - 14} 108 ${t - 10} 100 ${t - 14}Z" fill="${K.cyan}" opacity=".75"/>`;
      case 'leaf': return `<path d="M100 ${t + 4}C100 ${t - 8} 104 ${t - 14} 108 ${t - 18}" fill="none" stroke="${K.green}" stroke-width="3.5" stroke-linecap="round"/><path d="M106 ${t - 16}C108 ${t - 34} 124 ${t - 40} 134 ${t - 36}C132 ${t - 22} 120 ${t - 14} 106 ${t - 16}Z" fill="${K.green}"/>`;
      case 'cowlick': return `<path d="M96 ${t + 4}C92 ${t - 10} 86 ${t - 14} 80 ${t - 12}M100 ${t + 3}C100 ${t - 14} 104 ${t - 20} 110 ${t - 20}M104 ${t + 4}C110 ${t - 6} 116 ${t - 8} 120 ${t - 4}" fill="none" stroke="${P.rim}" stroke-width="4" stroke-linecap="round"/>`;
      case 'grass': return `<path d="M88 ${t + 6}L84 ${t - 10}M96 ${t + 4}L96 ${t - 16}M104 ${t + 4}L108 ${t - 14}M112 ${t + 6}L118 ${t - 6}" stroke="${K.green}" stroke-width="3.5" stroke-linecap="round"/>`;
      case 'feather': return `<path d="M100 ${t + 4}C98 ${t - 10} 104 ${t - 20} 114 ${t - 22}" fill="none" stroke="${P.rim}" stroke-width="4" stroke-linecap="round"/>`;
      case 'lure': return `<path d="M100 ${t + 4}C100 ${t - 22} 118 ${t - 34} 132 ${t - 26}" fill="none" stroke="${P.rim}" stroke-width="3" stroke-linecap="round"/><circle cx="134" cy="${t - 22}" r="12" fill="${K.coral}" opacity=".18"/><circle cx="134" cy="${t - 22}" r="6" fill="${K.coral}"/>`;
      case 'flower': return `<g transform="translate(100 ${t - 2})">${[0, 72, 144, 216, 288].map(a => `<ellipse cx="0" cy="-7" rx="4.5" ry="7" transform="rotate(${a})" fill="${K.coral}"/>`).join('')}<circle r="3.5" fill="${K.amber}"/></g>`;
      case 'cursor': return `<path d="${rr(92, t - 26, 16, 22, 3)}" fill="${K.coral}"/>`;
      default: return '';
    }
  }

  function eyes(type, b, P, a, eyeCol) {
    const y = b.eyeY + a.lookY, ex = b.ex, bl = a.blink, c = eyeCol || P.eye, o = [];
    const xs = [100 - ex + a.lookX, 100 + ex + a.lookX];
    const each = f => xs.forEach((x, i) => o.push(f(x, i)));
    switch (type) {
      case 'cursor': each(x => { const h = Math.max(2.5, 22 * bl); return `<path d="${rr(x - 6, y - h / 2, 12, h, 4)}" fill="${c}"/>`; }); break;
      case 'dot': each(x => `<ellipse cx="${n(x)}" cy="${y}" rx="6" ry="${n(Math.max(1.2, 6 * bl))}" fill="${c}"/>`); break;
      case 'round': each(x => `<ellipse cx="${n(x)}" cy="${y}" rx="11" ry="${n(Math.max(1.5, 11 * bl))}" fill="${P === PAL.ivory ? '#fff' : K.white}" stroke="${P.rim}" stroke-width="1.5"/>` + (bl > .4 ? `<circle cx="${n(x + a.lookX * .6 + 1)}" cy="${n(y + 1)}" r="5.5" fill="${K.ink}"/><circle cx="${n(x + 3)}" cy="${n(y - 2.5)}" r="1.6" fill="#fff"/>` : '')); break;
      case 'oval': each(x => `<ellipse cx="${n(x)}" cy="${y}" rx="7.5" ry="${n(Math.max(1.5, 12 * bl))}" fill="${c}"/>`); break;
      case 'slit': each(x => `<ellipse cx="${n(x)}" cy="${y}" rx="8.5" ry="${n(Math.max(1.5, 11 * bl))}" fill="${K.amber}" opacity=".95"/>` + (bl > .4 ? `<ellipse cx="${n(x)}" cy="${y}" rx="2.2" ry="${n(8 * bl)}" fill="${K.ink}"/>` : '')); break;
      case 'sleepy': each(x => `<path d="${rr(x - 7, y - 2, 14, Math.max(2, 9 * bl), 4)}" fill="${c}"/><path d="M${n(x - 8)} ${y - 3}H${n(x + 8)}" stroke="${c}" stroke-width="2.5" stroke-linecap="round"/>`); break;
      case 'ring': each(x => `<ellipse cx="${n(x)}" cy="${y}" rx="7.5" ry="${n(Math.max(1, 7.5 * bl))}" fill="none" stroke="${c}" stroke-width="4"/>`); break;
      case 'pixel': each(x => { const h = Math.max(2.5, 11 * bl); return `<rect x="${n(x - 5.5)}" y="${n(y - h / 2)}" width="11" height="${n(h)}" rx="1.5" fill="${c}"/>`; }); break;
      case 'happy': each(x => `<path d="M${n(x - 7)} ${y + 4}Q${n(x)} ${y - 9} ${n(x + 7)} ${y + 4}" fill="none" stroke="${c}" stroke-width="4.5" stroke-linecap="round"/>`); break;
      case 'glasses': each(x => `<circle cx="${n(x - a.lookX)}" cy="${b.eyeY}" r="11.5" fill="none" stroke="${K.coral}" stroke-width="2.5"/><ellipse cx="${n(x)}" cy="${y}" rx="4.5" ry="${n(Math.max(1, 4.5 * bl))}" fill="${c}"/>`); o.push(`<path d="M${100 - ex + 11.5} ${b.eyeY}H${100 + ex - 11.5}" stroke="${K.coral}" stroke-width="2.5"/>`); break;
      case 'visor': o.push(`<path d="${rr(100 - 32, b.eyeY - 11, 64, 22, 11)}" fill="${K.ink}" stroke="${P.rim}" stroke-width="2"/>`); each(x => `<ellipse cx="${n(x)}" cy="${b.eyeY}" rx="4.5" ry="${n(Math.max(1, 4.5 * bl))}" fill="${K.cyan}"/>`); break;
      case 'single': { const h = Math.max(3, 26 * bl); o.push(`<path d="${rr(100 + a.lookX - 7, y - h / 2, 14, h, 5)}" fill="${c}"/>`); break; }
      case 'singlering': o.push(`<circle cx="${100 + a.lookX}" cy="${y}" r="11" fill="none" stroke="${K.cyan}" stroke-width="4"/><circle cx="${100 + a.lookX * 1.5}" cy="${y}" r="${n(4 * bl + .5)}" fill="${K.cyan}"/>`); break;
      case 'prompt': o.push(`<path d="M${n(xs[0] - 5)} ${y - 8}L${n(xs[0] + 4)} ${y}L${n(xs[0] - 5)} ${y + 8}" fill="none" stroke="${c}" stroke-width="4.5" stroke-linecap="round" stroke-linejoin="round"/>`); o.push(`<path d="${rr(xs[1] - 7, y + 3, 14, 5, 2)}" fill="${K.coral}" opacity="${bl > .5 ? 1 : .15}"/>`); break;
    }
    return o.join('');
  }

  function spiral(cx, cy, r0, turns) {
    let d = ''; const N = 60;
    for (let i = 0; i <= N; i++) { const th = i / N * turns * 2 * Math.PI, r = r0 * (1 - i / N * .85); d += (i ? 'L' : 'M') + n(cx + r * Math.cos(th)) + ' ' + n(cy + r * Math.sin(th)); }
    return d;
  }

  function render(cfg, t, opts) {
    opts = opts || {};
    const mini = !!opts.mini, size = opts.size || 160;
    const b = B[cfg.body], P = PAL[cfg.pal || 'graphite'];
    const ph = cfg.phase || 0;
    const br = Math.sin(2 * Math.PI * (t / 3.6 + ph));
    const bt = ((t + ph * 4.2) % 4.2) / 4.2;
    const blink = bt < .035 ? 1 - bt / .035 : bt < .07 ? (bt - .035) / .035 : 1;
    const ew = ((t + ph * 5) % 5) / 5;
    const wig = ew > .55 && ew < .65 ? Math.sin((ew - .55) / .1 * Math.PI) * 10 : 0;
    const a = { blink: Math.max(.05, blink), lookX: 2.5 * Math.sin(2 * Math.PI * (t / 7 + ph)), lookY: 0, ear: wig };
    const sy = 1 + .028 * br, sx = 1 - .012 * br, rot = (cfg.sway ? 2.5 : .8) * Math.sin(2 * Math.PI * (t / 5.2 + ph)), fy = cfg.float ? -5 - 4 * Math.sin(2 * Math.PI * (t / 3 + ph)) : 0;
    const back = [], front = [], g = [];
    const ex = cfg.extras || [];
    const has = k => ex.includes(k);
    const st = `stroke="${P.rim}" stroke-width="2"`;
    // back layer
    if (has('aura')) back.push(`<circle cx="100" cy="${b.eyeY + 10}" r="70" fill="${K.cyan}" opacity=".08"/><circle cx="100" cy="${b.eyeY + 10}" r="56" fill="${K.cyan}" opacity=".07"/>`);
    if (has('mothwings') && !mini) back.push(`<ellipse cx="52" cy="${b.eyeY + 4}" rx="34" ry="28" transform="rotate(-20 52 ${b.eyeY + 4})" fill="${P.light}" ${st}/><ellipse cx="148" cy="${b.eyeY + 4}" rx="34" ry="28" transform="rotate(20 148 ${b.eyeY + 4})" fill="${P.light}" ${st}/><circle cx="46" cy="${b.eyeY}" r="8" fill="${K.coral}" opacity=".8"/><circle cx="154" cy="${b.eyeY}" r="8" fill="${K.coral}" opacity=".8"/>`);
    if (has('beewings') && !mini) back.push(`<ellipse cx="70" cy="${b.top + 14}" rx="20" ry="12" transform="rotate(-30 70 ${b.top + 14})" fill="${K.white}" opacity=".28" stroke="${K.white}" stroke-opacity=".5" stroke-width="1.5"/><ellipse cx="130" cy="${b.top + 14}" rx="20" ry="12" transform="rotate(30 130 ${b.top + 14})" fill="${K.white}" opacity=".28" stroke="${K.white}" stroke-opacity=".5" stroke-width="1.5"/>`);
    if (!mini) {
      const tw = 6 * Math.sin(2 * Math.PI * (t / 2.6 + ph));
      if (has('tail')) back.push(`<g transform="translate(146 160) rotate(${n(-tw)})"><path d="M0 0C16 0 26 -10 26 -28C26 -38 31 -45 38 -44" fill="none" stroke="${P.rim}" stroke-width="12" stroke-linecap="round"/><path d="M0 0C16 0 26 -10 26 -28C26 -38 31 -45 38 -44" fill="none" stroke="${P.body}" stroke-width="8.5" stroke-linecap="round"/></g>`);
      if (has('foxtail')) back.push(`<g transform="translate(144 158) rotate(${n(-tw)})"><path d="M0 0C24 6 44 -8 44 -34C44 -46 36 -52 30 -50C30 -30 20 -14 -2 -12Z" fill="${P.body}" ${st}/><path d="M44 -34C44 -46 36 -52 30 -50C30 -44 32 -38 36 -34C40 -32 43 -32 44 -34Z" fill="${K.white}" opacity=".9"/></g>`);
      if (has('thintail')) back.push(`<path transform="rotate(${n(-tw)} 148 164)" d="M148 164C170 168 178 150 172 140" fill="none" stroke="${P.rim}" stroke-width="3" stroke-linecap="round"/>`);
      if (has('birdtail')) back.push(`<path d="M144 150L170 132L172 146L176 152Z" fill="${P.rim}" stroke="${P.rim}" stroke-width="2" stroke-linejoin="round"/>`);
      if (has('glowtail')) back.push(`<circle cx="148" cy="158" r="${n(16 + 2 * br)}" fill="${K.cyan}" opacity=".18"/><circle cx="148" cy="158" r="9" fill="${K.cyan}"/>`);
    }
    // ears & toppers (behind body)
    if (cfg.ears && b.earX) {
      const m = ear(cfg.ears, P), w = (cfg.earTilt || 0) + a.ear;
      g.push(`<g transform="translate(${100 - b.earX} ${b.earY}) rotate(${n(-w)})">${m}</g><g transform="translate(${100 + b.earX} ${b.earY}) scale(-1 1) rotate(${n(-w)})">${m}</g>`);
    }
    if (cfg.top) g.push(topper(cfg.top, b, P));
    // body
    if (cfg.body === 'e') {
      g.push(`<path d="${b.ring}" fill="none" stroke="${P.rim}" stroke-width="40" stroke-linecap="round"/><path d="${b.ring}" fill="none" stroke="${P.body}" stroke-width="36" stroke-linecap="round"/><path d="M82 118H138" stroke="${K.coral}" stroke-width="11" stroke-linecap="round"/>`);
    } else if (cfg.body === 'stack') {
      g.push(`<path d="${b.d}" fill="${P.body}" ${st}/><path d="${b.mid}" fill="${P.light}" ${st}/><path d="${b.head}" fill="${P.body}" ${st}/><path d="M64 136h72" stroke="${K.coral}" stroke-width="5" stroke-linecap="round"/>`);
    } else {
      g.push(`<path d="${b.d}" fill="${P.body}" ${st}/>`);
      if (b.cap) g.push(`<path d="${b.cap}" fill="${cfg.body === 'acorn' ? P.rim : P.light}" ${st}/>`);
      if (cfg.body === 'acorn' && !mini) g.push(`<path d="M66 100l10-18M84 104l6-26M104 104l2-30M122 104l-4-26M138 100l-8-18" stroke="${P.body}" stroke-opacity=".5" stroke-width="2.5" stroke-linecap="round"/><path d="M100 66V56" stroke="${P.rim}" stroke-width="5" stroke-linecap="round"/>`);
      if (b.face) g.push(`<path d="${b.face}" fill="${P.light}"/>`);
      if (!mini && !b.cap && P !== PAL.ivory) g.push(`<path d="M${100 - b.hw * .76} ${b.eyeY}C${100 - b.hw * .72} ${b.top + 20} ${100 - b.hw * .45} ${b.top + 8} ${100 - b.hw * .12} ${b.top + 5}" fill="none" stroke="#fff" stroke-opacity=".07" stroke-width="7" stroke-linecap="round"/>`);
    }
    // body decor
    if (has('belly')) g.push(`<ellipse cx="100" cy="${n((b.eyeY + 172) / 2 + 14)}" rx="${n(b.hw * .55)}" ry="${n((172 - b.eyeY) * .3)}" fill="${P.light}"/>`);
    if (has('ivorybelly')) g.push(`<ellipse cx="100" cy="${n((b.eyeY + 172) / 2 + 10)}" rx="${n(b.hw * .62)}" ry="${n((172 - b.eyeY) * .38)}" fill="${K.white}"/>`);
    if (has('stripes')) g.push(`<path d="M${100 - b.hw * .92} ${b.eyeY + 30}Q100 ${b.eyeY + 40} ${100 + b.hw * .92} ${b.eyeY + 30}M${100 - b.hw * .82} ${b.eyeY + 50}Q100 ${b.eyeY + 60} ${100 + b.hw * .82} ${b.eyeY + 50}" fill="none" stroke="${K.coral}" stroke-width="9" stroke-linecap="round"/>`);
    if (has('spots') && !mini) g.push(`<circle cx="74" cy="132" r="5" fill="${K.coral}"/><circle cx="128" cy="140" r="6" fill="${K.coral}"/><circle cx="112" cy="156" r="4" fill="${K.coral}"/><circle cx="84" cy="156" r="4.5" fill="${K.coral}"/>`);
    if (has('capspots')) g.push(`<circle cx="74" cy="84" r="7" fill="${K.coral}"/><circle cx="108" cy="66" r="6" fill="${K.coral}"/><circle cx="136" cy="90" r="5" fill="${K.coral}"/>`);
    if (has('split')) g.push(`<path d="M100 ${b.top + 10}V170" stroke="${P.rim}" stroke-width="2.5"/>`);
    if (has('segments') && !mini) g.push(`<path d="M44 132Q100 120 156 132M42 150Q100 138 158 150" fill="none" stroke="${P.rim}" stroke-width="2.5"/>`);
    if (has('arms')) g.push(`<path d="${rr(42, 100, 18, 40, 9)}" fill="${P.body}" ${st}/><path d="${rr(140, 92, 18, 36, 9)}" fill="${P.body}" ${st}/>`);
    if (has('spines') && !mini) g.push(`<path d="M80 70l-4-4M120 70l4-4M70 140l-5 0M130 140l5 0M74 112l-5-2M126 112l5-2" stroke="${P.rim}" stroke-width="2" stroke-linecap="round"/>`);
    if (has('bolts')) g.push(`<circle cx="44" cy="${b.eyeY}" r="6" fill="${P.light}" ${st}/><circle cx="156" cy="${b.eyeY}" r="6" fill="${P.light}" ${st}/>`);
    if (has('shell')) g.push(`<circle cx="130" cy="112" r="36" fill="${P.light}" ${st}/><path d="${spiral(130, 112, 28, 2.2)}" fill="none" stroke="${K.coral}" stroke-width="4" stroke-linecap="round"/>`);
    // face
    const eyeCol = has('screen') ? K.cyan : null;
    if (has('screen')) g.push(`<path d="${rr(100 - b.ex - 18, b.eyeY - 20, 2 * b.ex + 36, 40, 12)}" fill="${K.ink}" stroke="${P.rim}" stroke-width="2"/>`);
    if (has('muzzle')) g.push(`<ellipse cx="100" cy="${b.eyeY + 20}" rx="17" ry="11" fill="${P === PAL.ivory ? '#fff' : P.light}"/><ellipse cx="100" cy="${b.eyeY + 15}" rx="4.5" ry="3.2" fill="${K.coral}"/>`);
    if (has('nose')) g.push(`<ellipse cx="100" cy="${b.eyeY + 15}" rx="4" ry="3" fill="${K.coral}"/>`);
    if (has('bignose')) g.push(`<ellipse cx="100" cy="${b.eyeY + 14}" rx="9" ry="7" fill="${K.coral}"/>`);
    g.push(eyes(cfg.eyes, b, P, a, eyeCol));
    if (has('cheeks')) g.push(`<ellipse cx="${100 - b.ex - 11}" cy="${b.eyeY + 14}" rx="6.5" ry="3.8" fill="${K.coral}" opacity=".6"/><ellipse cx="${100 + b.ex + 11}" cy="${b.eyeY + 14}" rx="6.5" ry="3.8" fill="${K.coral}" opacity=".6"/>`);
    if (has('beak')) g.push(`<path d="M92 ${b.eyeY + 12}H108L100 ${b.eyeY + 23}Z" fill="${K.coral}" stroke="${K.coral}" stroke-width="2" stroke-linejoin="round"/>`);
    if (has('bigbeak')) g.push(`<path d="M88 ${b.eyeY + 12}H112L100 ${b.eyeY + 32}Z" fill="${K.coral}" stroke="${K.coral}" stroke-width="3" stroke-linejoin="round"/><path d="M92 ${b.eyeY + 19}H108" stroke="${K.amber}" stroke-width="2.5"/>`);
    if (has('whiskers') && !mini) { const y = b.eyeY + 16, x0 = b.ex + 14; g.push(`<path d="M${100 - x0} ${y}l-20 -4M${100 - x0} ${y + 5}l-20 2M${100 + x0} ${y}l20 -4M${100 + x0} ${y + 5}l20 2" stroke="${P.eye}" stroke-opacity=".45" stroke-width="1.8" stroke-linecap="round"/>`); }
    // accent
    const acc = cfg.acc;
    const collar = (w, sw) => `<path d="M${100 - b.accW} ${b.accY}Q100 ${b.accY + 13} ${100 + b.accW} ${b.accY}" fill="none" stroke="${K.coral}" stroke-width="${sw}" stroke-linecap="round"/>`;
    if (acc === 'collar') g.push(collar(b.accW, mini ? 10 : 7));
    if (acc === 'tag') g.push(collar(b.accW, mini ? 10 : 6) + `<circle cx="100" cy="${b.accY + 13}" r="5" fill="${K.cyan}"/>`);
    if (acc === 'scarf') g.push(collar(b.accW, 10) + `<path d="M${100 + b.accW * .45} ${b.accY + 6}l7 22" stroke="${K.coral}" stroke-width="9" stroke-linecap="round"/>`);
    if (acc === 'led') g.push(`<path d="${rr(91, b.accY - 6, 18, 9, 4.5)}" fill="${K.coral}"/>`);
    if (acc === 'stripe') g.push(`<path d="${rr(96, b.top + 8, 8, 18, 4)}" fill="${K.coral}"/>`);
    if (acc === 'underline') g.push(`<path d="${rr(72, 156, 56, 9, 4)}" fill="${K.coral}"/>`);
    // front
    if (has('wings')) g.push(`<ellipse cx="${100 - b.hw + 2}" cy="${b.eyeY + 34}" rx="9" ry="20" transform="rotate(12 ${100 - b.hw + 2} ${b.eyeY + 34})" fill="${P.rim}"/><ellipse cx="${100 + b.hw - 2}" cy="${b.eyeY + 34}" rx="9" ry="20" transform="rotate(-12 ${100 + b.hw - 2} ${b.eyeY + 34})" fill="${P.rim}"/>`);
    if (has('flippers')) g.push(`<ellipse cx="48" cy="160" rx="16" ry="7" transform="rotate(-25 48 160)" fill="${P.rim}"/><ellipse cx="152" cy="160" rx="16" ry="7" transform="rotate(25 152 160)" fill="${P.rim}"/>`);
    if (!mini && !b.noFeet) {
      if (has('paws')) g.push(`<ellipse cx="84" cy="169" rx="11" ry="6" fill="${P.light}" ${st}/><ellipse cx="116" cy="169" rx="11" ry="6" fill="${P.light}" ${st}/>`);
      if (has('feet')) g.push(`<ellipse cx="86" cy="172" rx="9" ry="4.5" fill="${K.coral}"/><ellipse cx="114" cy="172" rx="9" ry="4.5" fill="${K.coral}"/>`);
      if (has('legs')) g.push(`<path d="${rr(76, 166, 12, 12, 3)}" fill="${P.rim}"/><path d="${rr(112, 166, 12, 12, 3)}" fill="${P.rim}"/>`);
    }
    if (has('orbit')) g.push(`<ellipse cx="100" cy="${b.eyeY + 22}" rx="76" ry="16" transform="rotate(-12 100 ${b.eyeY + 22})" fill="none" stroke="${K.cyan}" stroke-width="3"/><circle cx="${n(100 + 76 * Math.cos(t * 1.4 + ph * 6))}" cy="${n(b.eyeY + 22 + 16 * Math.sin(t * 1.4 + ph * 6) - 16 * Math.cos(t * 1.4 + ph * 6) * .21)}" r="5" fill="${K.cyan}"/>`);
    const tr = `translate(100 ${n(172 + fy)}) rotate(${n(rot)}) scale(${n(sx)} ${n(sy)}) translate(-100 -172)`;
    const sh = mini ? '' : `<ellipse cx="100" cy="176" rx="${n(46 * (1 + fy / 60))}" ry="5" fill="#000" opacity="${n(.3 * (1 + fy / 40))}"/>`;
    const vb = mini ? '30 26 140 152' : '0 0 200 200';
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="${vb}" style="display:block;overflow:visible">${sh}<g transform="${tr}">${back.join('')}${g.join('')}${front.join('')}</g></svg>`;
  }

  const F = {
    A: 'Cats and foxes', B: 'Owls and birds', C: 'Small mammals', D: 'Spirits and blobs',
    E: 'Robots and devices', F: 'Plants and fungi', G: 'Bugs and critters', H: 'Glyph creatures'
  };
  const L = [
    ['A1', 'Tabby', { body: 'round', ears: 'cat', eyes: 'slit', acc: 'tag', extras: ['whiskers', 'tail', 'paws'] }],
    ['A2', 'Fox', { body: 'egg', ears: 'fox', eyes: 'dot', acc: 'scarf', extras: ['muzzle', 'foxtail', 'paws'] }],
    ['A3', 'Lynx', { body: 'dome', ears: 'lynx', eyes: 'round', extras: ['cheeks', 'whiskers', 'tail', 'paws'] }],
    ['A4', 'Fennec', { body: 'bean', ears: 'fennec', eyes: 'dot', acc: 'collar', extras: ['nose', 'foxtail', 'paws'] }],
    ['A5', 'Ivory cat', { body: 'dome', ears: 'cat', eyes: 'cursor', acc: 'collar', pal: 'ivory', extras: ['tail', 'paws'] }],
    ['A6', 'Drowsy cat', { body: 'pear', ears: 'cat', eyes: 'sleepy', acc: 'scarf', extras: ['whiskers', 'tail', 'paws'] }],
    ['A7', 'Kit', { body: 'round', ears: 'fox', eyes: 'cursor', acc: 'tag', pal: 'slate', extras: ['foxtail', 'paws'] }],
    ['B1', 'Owl', { body: 'owl', ears: 'tuft', eyes: 'round', extras: ['beak', 'wings', 'feet', 'belly'] }],
    ['B2', 'Owlet', { body: 'egg', ears: 'tuft', eyes: 'ring', acc: 'collar', extras: ['beak', 'wings'] }],
    ['B3', 'Wren', { body: 'round', eyes: 'dot', top: 'feather', extras: ['beak', 'cheeks', 'wings', 'birdtail', 'feet'] }],
    ['B4', 'Night owl', { body: 'owl', ears: 'tuft', eyes: 'glasses', pal: 'steel', extras: ['beak', 'wings', 'feet'] }],
    ['B5', 'Puffin', { body: 'tall', eyes: 'oval', extras: ['bigbeak', 'ivorybelly', 'feet', 'wings'] }],
    ['B6', 'Penguin', { body: 'egg', eyes: 'dot', pal: 'deep', extras: ['beak', 'ivorybelly', 'wings', 'feet'] }],
    ['B7', 'Chick', { body: 'round', eyes: 'dot', top: 'feather', pal: 'ivory', extras: ['beak', 'cheeks', 'feet'] }],
    ['C1', 'Seal', { body: 'bean', eyes: 'round', extras: ['whiskers', 'nose', 'flippers'] }],
    ['C2', 'Otter', { body: 'pear', ears: 'round', eyes: 'dot', extras: ['muzzle', 'whiskers', 'tail', 'paws'] }],
    ['C3', 'Mole', { body: 'wide', eyes: 'sleepy', acc: 'collar', extras: ['bignose', 'paws'] }],
    ['C4', 'Hamster', { body: 'round', ears: 'round', eyes: 'dot', extras: ['cheeks', 'belly', 'paws'] }],
    ['C5', 'Bear cub', { body: 'dome', ears: 'round', eyes: 'dot', acc: 'collar', extras: ['muzzle', 'paws'] }],
    ['C6', 'Mouse', { body: 'egg', ears: 'biground', eyes: 'dot', pal: 'slate', extras: ['nose', 'whiskers', 'thintail', 'paws'] }],
    ['C7', 'Rabbit', { body: 'egg', ears: 'bunny', eyes: 'cursor', extras: ['nose', 'cheeks', 'paws'] }],
    ['D1', 'Mochi', { body: 'bean', eyes: 'cursor', extras: ['cheeks'] }],
    ['D2', 'Droplet', { body: 'drop', eyes: 'cursor', acc: 'collar', sway: 1 }],
    ['D3', 'Ghost', { body: 'ghost', eyes: 'oval', extras: ['cheeks'], float: 1 }],
    ['D4', 'Wisp', { body: 'drop', eyes: 'single', acc: 'stripe', pal: 'slate', extras: ['aura'], float: 1, sway: 1 }],
    ['D5', 'Pebble', { body: 'wide', eyes: 'sleepy', extras: ['belly'] }],
    ['D6', 'Puff', { body: 'round', eyes: 'dot', top: 'cowlick', extras: ['cheeks'] }],
    ['D7', 'Lantern', { body: 'egg', eyes: 'round', top: 'lure', pal: 'deep', extras: ['flippers'] }],
    ['E1', 'Terminal', { body: 'square', ears: 'antennae', eyes: 'cursor', extras: ['screen', 'legs'] }],
    ['E2', 'Visor', { body: 'round', top: 'antenna', eyes: 'visor', acc: 'led', extras: ['legs'] }],
    ['E3', 'Cube', { body: 'square', eyes: 'pixel', acc: 'led', extras: ['bolts', 'legs'] }],
    ['E4', 'Capsule', { body: 'tall', top: 'antenna', eyes: 'singlering', acc: 'collar', extras: ['legs'] }],
    ['E5', 'Monitor', { body: 'square', eyes: 'dot', acc: 'underline', pal: 'steel', extras: ['screen', 'legs'] }],
    ['E6', 'Drone', { body: 'wide', ears: 'antennae', eyes: 'visor', extras: ['beewings'], float: 1 }],
    ['E7', 'Ivory bot', { body: 'square', top: 'antenna', eyes: 'cursor', pal: 'ivory', acc: 'led', extras: ['screen', 'bolts'] }],
    ['F1', 'Sprout', { body: 'egg', top: 'sprout', eyes: 'dot', extras: ['cheeks'] }],
    ['F2', 'Seedling', { body: 'drop', top: 'leaf', eyes: 'cursor', acc: 'collar', sway: 1 }],
    ['F3', 'Toadstool', { body: 'mushroom', eyes: 'dot', extras: ['capspots', 'cheeks'] }],
    ['F4', 'Acorn', { body: 'acorn', eyes: 'cursor', acc: 'collar' }],
    ['F5', 'Moss', { body: 'bean', top: 'grass', eyes: 'sleepy', extras: ['belly'] }],
    ['F6', 'Bud', { body: 'round', top: 'budcyan', eyes: 'cursor', acc: 'collar' }],
    ['F7', 'Cactus', { body: 'tall', top: 'flower', eyes: 'dot', extras: ['arms', 'spines', 'cheeks'] }],
    ['G1', 'Snail', { body: 'bean', ears: 'antennae', eyes: 'cursor', extras: ['shell'] }],
    ['G2', 'Beetle', { body: 'round', ears: 'moth', eyes: 'dot', extras: ['split', 'legs'] }],
    ['G3', 'Moth', { body: 'egg', ears: 'moth', eyes: 'round', acc: 'collar', extras: ['mothwings'] }],
    ['G4', 'Ladybird', { body: 'round', ears: 'antennae', eyes: 'dot', extras: ['spots', 'split', 'legs'] }],
    ['G5', 'Bee', { body: 'egg', ears: 'antennae', eyes: 'oval', extras: ['stripes', 'beewings'], float: 1 }],
    ['G6', 'Firefly', { body: 'drop', ears: 'moth', eyes: 'dot', extras: ['glowtail', 'beewings'], float: 1 }],
    ['G7', 'Pill bug', { body: 'wide', ears: 'horns', eyes: 'sleepy', pal: 'slate', extras: ['segments', 'legs'] }],
    ['H1', 'The “e”', { body: 'e', ears: 'cat', eyes: 'dot' }],
    ['H2', 'Cursor', { body: 'block', eyes: 'slit', acc: 'underline', pal: 'deep' }],
    ['H3', 'Chevron', { body: 'dome', ears: 'chevron', eyes: 'cursor', acc: 'collar', extras: ['paws'] }],
    ['H4', 'Prompt', { body: 'wide', eyes: 'prompt', ears: 'round', extras: ['paws'] }],
    ['H5', 'Node', { body: 'round', eyes: 'dot', acc: 'stripe', extras: ['orbit'], float: 1 }],
    ['H6', 'Stack', { body: 'stack', ears: 'cat', eyes: 'cursor' }],
    ['H7', 'Keycap', { body: 'keycap', ears: 'bolt', eyes: 'cursor', top: 'cursor', extras: ['legs'] }]
  ];
  const OPTIONS = L.map(([id, name, cfg], i) => ({ id, name, family: F[id[0]], cfg: Object.assign({ phase: (i * .137) % 1 }, cfg) }));
  window.ElisaOptions = { OPTIONS, FAMILIES: F, render };
})();
