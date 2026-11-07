(function () {
  const EASE = {
    lin: t => t,
    io: t => (t < .5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2),
    out: t => 1 - Math.pow(1 - t, 3),
    in: t => t * t * t,
    back: t => { const c = 1.9; return 1 + (c + 1) * Math.pow(t - 1, 3) + c * Math.pow(t - 1, 2); },
    step: t => 0
  };
  const EASE_INFO = {
    io: ['Ease in-out', 'cubic-bezier(.65,0,.35,1)', 'Breathing, looks, weight shifts'],
    out: ['Ease out', 'cubic-bezier(.33,1,.68,1)', 'Reactions, pops, landing'],
    in: ['Ease in', 'cubic-bezier(.32,0,.67,0)', 'Anticipation, falling, leaving'],
    back: ['Overshoot', 'cubic-bezier(.34,1.7,.64,1)', 'Ears and badges settling'],
    lin: ['Linear', 'linear', 'Progress, orbiting, drifting z']
  };
  const COL = { body: '#34343F', rim: '#50505F', paw: '#44444F', eye: '#E8E8E8', coral: '#D77757', cyan: '#82C8DC', green: '#78C878', red: '#F06464', amber: '#E6BE50', sec: '#9696A0', ink: '#1E1E24' };
  const DEF = { x: 0, y: 0, sx: 1, sy: 1, rot: 0, earL: 0, earR: 0, eyeOpen: 1, lookX: 0, lookY: 0, eye: 'block', tail: 0, pawR: 0, glow: 0, dim: 0, p1: 0, p2: 0, p3: 0 };

  const STATES = {
    idle: { label: 'Idle', dur: 4000, loop: true, use: 'Waiting for input. Slow breath, a blink, one ear flick.', tracks: {
      sy: [[0, 1], [.5, 1.035], [1, 1]], sx: [[0, 1], [.5, .985], [1, 1]],
      eyeOpen: [[0, 1], [.78, 1, 'in'], [.8, .05, 'out'], [.83, 1], [1, 1]],
      earR: [[0, 0], [.45, 0, 'out'], [.5, -16, 'back'], [.6, 0], [1, 0]],
      tail: [[0, 0], [.5, 10], [1, 0]], lookX: [[0, 0], [.25, 0], [.32, -3], [.6, -3], [.68, 0], [1, 0]] } },
    listening: { label: 'Listening', dur: 2000, loop: true, use: 'User is typing. Head tilts toward the input, small nods.', tracks: {
      rot: [[0, 6], [.5, 9], [1, 6]], lookX: [[0, -4]], lookY: [[0, 5]],
      y: [[0, 0], [.25, 2.5], [.5, 0], [.75, 2.5], [1, 0]],
      earL: [[0, -8], [.5, -13], [1, -8]], earR: [[0, -8], [.5, -13], [1, -8]] } },
    thinking: { label: 'Thinking', dur: 2400, loop: true, use: 'Model is reasoning. Looks up, tail taps, three cyan beads.', tracks: {
      lookX: [[0, 4]], lookY: [[0, -6]], rot: [[0, -4], [.5, -6], [1, -4]], eyeOpen: [[0, .8]],
      sy: [[0, 1], [.5, 1.02], [1, 1]],
      tail: [[0, 0], [.2, 14, 'out'], [.3, 0], [.6, 0], [.7, 14, 'out'], [.8, 0], [1, 0]],
      p1: [[0, 0], [.1, 1], [.7, 1], [.85, 0], [1, 0]], p2: [[0, 0], [.25, 0], [.35, 1], [.75, 1], [.9, 0], [1, 0]], p3: [[0, 0], [.45, 0], [.55, 1], [.8, 1], [.95, 0], [1, 0]] } },
    streaming: { label: 'Streaming', dur: 2400, loop: true, use: 'Writing a reply. Bobs on the beat while lines type out.', tracks: {
      y: [[0, 0], [.25, -3], [.5, 0], [.75, -3], [1, 0]], sy: [[0, 1], [.25, 1.025], [.5, 1], [.75, 1.025], [1, 1]],
      lookX: [[0, 5]], eyeOpen: [[0, .85]], p1: [[0, 0, 'lin'], [.9, 1], [1, 1]] } },
    running: { label: 'Running tool', dur: 1600, loop: true, use: 'A tool is executing. Holds a small terminal, watches progress.', tracks: {
      lookX: [[0, 6]], lookY: [[0, 3]], eyeOpen: [[0, .9]], rot: [[0, 2], [.5, 4], [1, 2]], pawR: [[0, .45]],
      earR: [[0, 0], [.5, -6], [1, 0]], p1: [[0, 0, 'lin'], [1, 1]] } },
    permission: { label: 'Waiting for permission', dur: 2000, loop: true, use: 'Needs a yes or no. Sits up, paw raised, looks at you.', tracks: {
      sy: [[0, 1.05]], y: [[0, -2]], pawR: [[0, 1], [.5, .8], [1, 1]], earL: [[0, -10]], earR: [[0, -10]],
      eye: [[0, 'wide']], rot: [[0, -3], [.5, 3], [1, -3]], p1: [[0, 1], [.5, 1.12], [1, 1]] } },
    success: { label: 'Success', dur: 1600, loop: true, use: 'Task done. Squat, hop, happy eyes, green check.', tracks: {
      y: [[0, 0], [.12, 3, 'out'], [.32, -24, 'in'], [.52, 0, 'out'], [.62, 2], [.72, 0], [1, 0]],
      sy: [[0, 1], [.12, .9, 'out'], [.3, 1.08], [.52, .92, 'out'], [.62, 1.02], [.72, 1], [1, 1]],
      sx: [[0, 1], [.12, 1.08, 'out'], [.3, .95], [.52, 1.06, 'out'], [.62, .99], [.72, 1], [1, 1]],
      eye: [[0, 'happy']], earL: [[0, 0], [.3, -12], [.6, 0, 'back'], [1, 0]], earR: [[0, 0], [.3, -12], [.6, 0, 'back'], [1, 0]],
      p1: [[0, 0], [.3, 0, 'back'], [.45, 1], [.85, 1, 'in'], [1, 0]], p2: [[0, 0], [.32, 0, 'out'], [.7, 1], [1, 1]] } },
    error: { label: 'Error', dur: 2000, loop: true, use: 'Something failed. Ears droop, a small head shake, red badge.', tracks: {
      eye: [[0, 'sad']], earL: [[0, 24]], earR: [[0, 24]], sy: [[0, .96]], lookY: [[0, 4]], tail: [[0, -18]],
      x: [[0, 0], [.08, -4], [.16, 4], [.24, -3], [.32, 2], [.4, 0], [1, 0]],
      p1: [[0, .9], [.1, 1.1, 'out'], [.2, 1], [1, 1]] } },
    interrupted: { label: 'Interrupted', dur: 1400, loop: true, use: 'User pressed stop. Startled hop back, freeze, settle.', tracks: {
      x: [[0, 0, 'out'], [.1, -8], [.6, -8], [1, 0]], y: [[0, 0, 'out'], [.08, -9, 'in'], [.18, 0], [1, 0]],
      eye: [[0, 'wide']], eyeOpen: [[0, 1], [.08, 1.15], [.6, 1.15], [1, 1]],
      earL: [[0, 0, 'out'], [.08, -20], [.6, -20], [1, 0]], earR: [[0, 0, 'out'], [.08, -20], [.6, -20], [1, 0]],
      rot: [[0, 0, 'out'], [.08, -6], [.6, -6], [1, 0]], p1: [[0, 0, 'back'], [.1, 1], [.7, 1, 'in'], [.9, 0], [1, 0]] } },
    memory: { label: 'Memory recalled', dur: 2400, loop: true, use: 'A stored fact comes back. Cyan bead rises, sinks in, eyes glow.', tracks: {
      p1: [[0, 0], [.55, 1, 'in'], [.7, 1.2], [1, 1.2]], p2: [[0, 0], [.08, 1], [.65, 1], [.72, 0], [1, 0]], p3: [[0, 0], [.68, 0, 'out'], [1, 1]],
      glow: [[0, 0], [.68, 0, 'out'], [.76, 1], [1, 0]], lookY: [[0, 4], [.5, -7], [.7, -7], [1, 0]],
      earL: [[0, 0], [.68, 0, 'back'], [.78, -12], [1, 0]], earR: [[0, 0], [.68, 0, 'back'], [.78, -12], [1, 0]] } },
    sleeping: { label: 'Unloading / sleeping', dur: 4000, loop: true, use: 'Weights moved to disk. Curled low, eyes shut, slow z.', tracks: {
      eye: [[0, 'closed']], sy: [[0, .86], [.5, .9], [1, .86]], sx: [[0, 1.06], [.5, 1.04], [1, 1.06]],
      earL: [[0, 20]], earR: [[0, 20]], dim: [[0, .45]], tail: [[0, 38]], p1: [[0, 0, 'lin'], [1, 1]] } },
    waking: { label: 'Waking / restore', dur: 2400, loop: true, use: 'Weights back in memory. Stretches up, double blink, ears pop.', tracks: {
      sy: [[0, .86], [.25, .86, 'in'], [.45, 1.12, 'out'], [.6, .97], [.7, 1], [1, 1]],
      sx: [[0, 1.06], [.25, 1.06, 'in'], [.45, .94, 'out'], [.6, 1.01], [.7, 1], [1, 1]],
      eye: [[0, 'closed', 'step'], [.45, 'block']],
      eyeOpen: [[0, 1], [.45, .1, 'out'], [.52, 1], [.6, 1, 'in'], [.62, .1, 'out'], [.65, 1], [.7, 1, 'in'], [.72, .1, 'out'], [.75, 1], [1, 1]],
      earL: [[0, 20], [.4, 20, 'back'], [.52, -12], [.65, 0], [1, 0]], earR: [[0, 20], [.42, 20, 'back'], [.54, -12], [.67, 0], [1, 0]],
      dim: [[0, .45], [.45, .45], [.6, 0], [1, 0]], tail: [[0, 38], [.4, 38], [.6, 0], [1, 0]], p1: [[0, 0], [.45, 0, 'out'], [.8, 1], [1, 1]] } }
  };
  const ORDER = ['idle', 'listening', 'thinking', 'streaming', 'running', 'permission', 'success', 'error', 'interrupted', 'memory', 'sleeping', 'waking'];

  function val(k, t) {
    if (!Array.isArray(k)) return k;
    if (k.length === 1 || t <= k[0][0]) return k[0][1];
    for (let i = 0; i < k.length - 1; i++) {
      const a = k[i], b = k[i + 1];
      if (t <= b[0]) {
        if (typeof a[1] !== 'number') return a[1];
        const u = (t - a[0]) / ((b[0] - a[0]) || 1);
        return a[1] + (b[1] - a[1]) * EASE[a[2] || 'io'](u);
      }
    }
    return k[k.length - 1][1];
  }
  function pose(state, t) {
    const s = STATES[state] || STATES.idle, p = Object.assign({}, DEF);
    for (const key in s.tracks) p[key] = val(s.tracks[key], t);
    return p;
  }
  const n = v => Math.round(v * 100) / 100;
  function rr(x, y, w, h, r) {
    r = Math.min(r, w / 2, h / 2);
    return `M${n(x + r)} ${n(y)}H${n(x + w - r)}A${n(r)} ${n(r)} 0 0 1 ${n(x + w)} ${n(y + r)}V${n(y + h - r)}A${n(r)} ${n(r)} 0 0 1 ${n(x + w - r)} ${n(y + h)}H${n(x + r)}A${n(r)} ${n(r)} 0 0 1 ${n(x)} ${n(y + h - r)}V${n(y + r)}A${n(r)} ${n(r)} 0 0 1 ${n(x + r)} ${n(y)}Z`;
  }
  function mix(a, b, t) {
    const pa = parseInt(a.slice(1), 16), pb = parseInt(b.slice(1), 16);
    const c = [16, 8, 0].map(s => Math.round(((pa >> s) & 255) * (1 - t) + ((pb >> s) & 255) * t));
    return '#' + c.map(v => v.toString(16).padStart(2, '0')).join('');
  }
  const BODY = 'M44 150C44 98 70 68 100 68C130 68 156 98 156 150C156 164 148 172 134 172H66C52 172 44 164 44 150Z';
  const EAR = 'M-13 6C-16 -14 -12 -30 -6 -32C0 -33 8 -20 13 2Z';
  const EAR_IN = 'M-8 2C-10 -11 -8 -21 -5 -23C-1 -23 3 -14 7 0Z';

  function eyes(p, mini, color) {
    const cx = [84, 116], cy = 112 + p.lookY, out = [];
    const sw = mini ? 6 : 4.5;
    cx.forEach((x0, i) => {
      const x = x0 + p.lookX;
      if (p.eye === 'happy') out.push(`<path d="M${x - 7} ${cy + 4}Q${x} ${cy - 9} ${x + 7} ${cy + 4}" fill="none" stroke="${color}" stroke-width="${sw}" stroke-linecap="round"/>`);
      else if (p.eye === 'closed') out.push(`<path d="M${x - 7} ${cy}Q${x} ${cy + 5} ${x + 7} ${cy}" fill="none" stroke="${color}" stroke-width="${sw * .9}" stroke-linecap="round"/>`);
      else {
        const wide = p.eye === 'wide', sad = p.eye === 'sad';
        const w = (mini ? 16 : wide ? 14 : 12), h0 = (mini ? 26 : wide ? 26 : 22) * (sad ? .55 : 1);
        const h = Math.max(2.5, h0 * Math.min(1.2, p.eyeOpen));
        out.push(`<path d="${rr(x - w / 2, cy - h / 2 + (sad ? 3 : 0), w, h, mini ? 5 : 4)}" fill="${color}"/>`);
        if (wide && !mini) out.push(`<circle cx="${n(x + 2.5)}" cy="${n(cy - h / 2 + 5)}" r="2" fill="${COL.ink}" opacity=".6"/>`);
        if (sad) { const d = i ? -1 : 1; out.push(`<path d="M${x - 7 * d} ${cy - 9}L${x + 6 * d} ${cy - 13}" stroke="${color}" stroke-width="3" stroke-linecap="round"/>`); }
      }
    });
    return out.join('');
  }

  function props(state, p, ms) {
    const o = [];
    if (state === 'thinking') [[140, 52, 4, p.p1], [154, 37, 5.5, p.p2], [172, 26, 7, p.p3]].forEach(([x, y, r, a]) => o.push(`<circle cx="${x}" cy="${n(y - a * 3)}" r="${r}" fill="${COL.cyan}" opacity="${n(a)}"/>`));
    if (state === 'streaming') {
      const W = [30, 26, 18], tot = W.reduce((a, b) => a + b); let left = p.p1 * tot, cur = null;
      W.forEach((w, i) => { const l = Math.max(0, Math.min(w, left)); left -= l; const y = 92 + i * 12; if (l > 0) o.push(`<path d="M164 ${y}h${n(l)}" stroke="${COL.eye}" stroke-opacity=".85" stroke-width="4" stroke-linecap="round"/>`); if (cur === null && (l < w || i === 2)) cur = [164 + l + 4, y]; });
      if (cur && Math.floor(ms / 280) % 2 === 0) o.push(`<rect x="${n(cur[0])}" y="${cur[1] - 5}" width="4.5" height="10" rx="1" fill="${COL.coral}"/>`);
    }
    if (state === 'running') {
      o.push(`<path d="${rr(148, 98, 46, 34, 5)}" fill="${COL.ink}" stroke="${COL.cyan}" stroke-width="2"/>`);
      o.push(`<path d="M155 107l4 3.5-4 3.5M162 115h6" fill="none" stroke="${COL.cyan}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>`);
      o.push(`<path d="M155 124h32" stroke="#46465A" stroke-width="3" stroke-linecap="round"/><path d="M155 124h${n(Math.max(.1, 32 * p.p1))}" stroke="${COL.cyan}" stroke-width="3" stroke-linecap="round"/>`);
    }
    if (state === 'permission') o.push(`<g transform="translate(46 46) scale(${n(p.p1)})"><circle r="15" fill="${COL.ink}" stroke="${COL.amber}" stroke-width="2.5"/><path d="M-4.5 -4a4.5 4.5 0 1 1 6.5 4c-1.3.7-2 1.5-2 3" fill="none" stroke="${COL.amber}" stroke-width="2.6" stroke-linecap="round"/><circle cy="7.5" r="1.7" fill="${COL.amber}"/></g>`);
    if (state === 'success') {
      if (p.p2 > 0 && p.p2 < 1) o.push(`<circle cx="152" cy="50" r="${n(13 + 16 * p.p2)}" fill="none" stroke="${COL.green}" stroke-width="2" opacity="${n(1 - p.p2)}"/>`);
      if (p.p1 > .01) o.push(`<g transform="translate(152 50) scale(${n(p.p1)})"><circle r="13" fill="${COL.green}"/><path d="M-6 0l4 4 8-8" fill="none" stroke="${COL.ink}" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"/></g>`);
    }
    if (state === 'error') o.push(`<g transform="translate(152 52) scale(${n(p.p1)})"><circle r="12" fill="${COL.red}"/><path d="M0 -6v6" stroke="${COL.ink}" stroke-width="3" stroke-linecap="round"/><circle cy="5.5" r="1.7" fill="${COL.ink}"/></g>`);
    if (state === 'interrupted' && p.p1 > .01) o.push(`<g transform="translate(150 50) scale(${n(p.p1)})"><path d="${rr(-12, -12, 24, 24, 6)}" fill="${COL.amber}"/><path d="${rr(-4.5, -4.5, 9, 9, 1.5)}" fill="${COL.ink}"/></g>`);
    if (state === 'memory') {
      const s = p.p1; let x, y;
      if (s <= 1) { x = 100 + 22 * Math.sin(s * Math.PI); y = 142 - 112 * s; } else { x = 100; y = 30 + 62 * (s - 1) / .2; }
      if (p.p2 > .01) o.push(`<circle cx="${n(x)}" cy="${n(y)}" r="13" fill="${COL.cyan}" opacity="${n(.22 * p.p2)}"/><circle cx="${n(x)}" cy="${n(y)}" r="6.5" fill="${COL.cyan}" opacity="${n(p.p2)}"/>`);
      if (p.p3 > 0 && p.p3 < 1) o.push(`<circle cx="100" cy="104" r="${n(30 + 44 * p.p3)}" fill="none" stroke="${COL.cyan}" stroke-width="2" opacity="${n((1 - p.p3) * .8)}"/>`);
    }
    if (state === 'sleeping') for (let i = 0; i < 3; i++) {
      const ph = (p.p1 + i / 3) % 1, x = 140 + 20 * ph + 3 * Math.sin(ph * 6), y = 92 - 52 * ph, s = .7 + .7 * ph;
      o.push(`<path transform="translate(${n(x)} ${n(y)}) scale(${n(s)})" d="M-4 -4h8l-8 8h8" fill="none" stroke="${COL.sec}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" opacity="${n(Math.sin(ph * Math.PI))}"/>`);
    }
    if (state === 'waking' && p.p1 > 0 && p.p1 < 1) o.push(`<circle cx="100" cy="124" r="${n(52 + 34 * p.p1)}" fill="none" stroke="${COL.cyan}" stroke-width="2" opacity="${n((1 - p.p1) * .9)}"/>`);
    return o.join('');
  }

  function figure(p, mini) {
    const g = [];
    const body = mini ? '#4A4A58' : COL.body;
    if (!mini) g.push(`<g transform="translate(146 160) rotate(${n(-p.tail)})"><path d="M0 0C16 0 26 -10 26 -28C26 -38 31 -45 38 -44" fill="none" stroke="${COL.rim}" stroke-width="12" stroke-linecap="round"/><path d="M0 0C16 0 26 -10 26 -28C26 -38 31 -45 38 -44" fill="none" stroke="${body}" stroke-width="8.5" stroke-linecap="round"/></g>`);
    [[76, -p.earL, 1], [124, p.earR, -1]].forEach(([x, a, m]) => g.push(`<g transform="translate(${x} 80) rotate(${n(a)}) scale(${m} 1)"><path d="${EAR}" fill="${body}" stroke="${mini ? body : COL.rim}" stroke-width="2" stroke-linejoin="round"/><path d="${EAR_IN}" fill="${COL.coral}" opacity="${mini ? 1 : .85}"/></g>`));
    g.push(`<path d="${BODY}" fill="${body}" stroke="${mini ? body : COL.rim}" stroke-width="2"/>`);
    if (!mini) g.push(`<path d="M62 110C64 90 78 77 94 73" fill="none" stroke="#fff" stroke-opacity=".07" stroke-width="7" stroke-linecap="round"/>`);
    g.push(`<path d="M56 138Q100 151 144 138" fill="none" stroke="${COL.coral}" stroke-width="${mini ? 10 : 7}" stroke-linecap="round"/>`);
    g.push(eyes(p, mini, mix(COL.eye, COL.cyan, p.glow)));
    if (!mini) {
      g.push(`<ellipse cx="84" cy="169" rx="11" ry="6" fill="${COL.paw}" stroke="${COL.rim}" stroke-width="1.5"/>`);
      const px = 116 + (134 - 116) * p.pawR, py = 169 + (126 - 169) * p.pawR;
      g.push(`<ellipse cx="${n(px)}" cy="${n(py)}" rx="11" ry="6" transform="rotate(${n(-35 * p.pawR)} ${n(px)} ${n(py)})" fill="${COL.paw}" stroke="${COL.rim}" stroke-width="1.5"/>`);
    }
    return g.join('');
  }

  function svg(state, t, opts) {
    opts = opts || {};
    const s = STATES[state] || STATES.idle;
    const u = ((t % 1) + 1) % 1, ms = u * s.dur;
    const p = pose(state, u);
    const detail = opts.detail || 'full', mini = detail === 'mini', size = opts.size || 160;
    if (detail === 'template') return template(p, size, opts.color || '#000');
    const yk = mini ? .45 : 1;
    const tr = `translate(${n(100 + p.x * yk)} ${n(172 + p.y * yk)}) rotate(${n(p.rot)}) scale(${n(p.sx)} ${n(p.sy)}) translate(-100 -172)`;
    const lift = Math.max(0, -p.y);
    const sh = mini ? '' : `<ellipse cx="${n(100 + p.x)}" cy="175" rx="${n(48 * p.sx * (1 - lift / 70))}" ry="5" fill="#000" opacity="${n(.32 * (1 - lift / 50))}"/>`;
    const vb = mini ? '38 36 124 140' : '0 0 200 200';
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="${vb}" style="display:block;overflow:visible">${sh}<g transform="${tr}" opacity="${n(1 - p.dim * .35)}">${figure(p, mini)}</g>${mini || opts.noProps ? '' : props(state, p, ms)}</svg>`;
  }

  function template(p, size, color) {
    const tr = `translate(100 172) rotate(${n(p.rot * .5)}) scale(${n(p.sx)} ${n(p.sy)}) translate(-100 -172)`;
    const cy = 112 + p.lookY * .6;
    let holes = '';
    [84, 116].forEach(x0 => {
      const x = x0 + p.lookX * .6;
      if (p.eye === 'closed') holes += rr(x - 9, cy - 2.5, 18, 6, 3);
      else { const h = Math.max(6, 28 * Math.min(1, p.eyeOpen)); holes += rr(x - 8.5, cy - h / 2, 17, h, 5); }
    });
    const ears = [[76, -p.earL, 1], [124, p.earR, -1]].map(([x, a, m]) => `<path transform="translate(${x} 80) rotate(${n(a)}) scale(${m} 1)" d="${EAR}" fill="${color}"/>`).join('');
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="38 40 124 136" style="display:block"><g transform="${tr}">${ears}<path d="${BODY}${holes}" fill="${color}" fill-rule="evenodd"/></g></svg>`;
  }

  function spec(state) {
    const s = STATES[state];
    return { name: state, label: s.label, durationMs: s.dur, loop: s.loop, fps: 60, viewBox: [0, 0, 200, 200], pivot: { body: [100, 172], earL: [76, 80], earR: [124, 80], tail: [146, 160] },
      tracks: Object.fromEntries(Object.entries(s.tracks).map(([k, v]) => [k, v.map(([t, value, ease]) => ({ t, ms: Math.round(t * s.dur), value, ease: ease || 'io' }))])) };
  }

  window.ElisaChar = { STATES, ORDER, EASE, EASE_INFO, COL, pose, svg, spec, props, rr, mix };
})();
