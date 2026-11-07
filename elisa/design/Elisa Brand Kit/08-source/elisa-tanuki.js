(function () {
  const EC = window.ElisaChar;
  const { STATES, ORDER, EASE, EASE_INFO, pose, rr, mix } = EC;
  const n = v => Math.round(v * 100) / 100;
  const COL = Object.assign({}, EC.COL, { body: '#6A6A78', rim: '#484852', mini: '#74748A', mask: '#3A3A42', paw: '#62626E', belly: '#ECE6DA', iris: '#E6BE50', white: '#FFFFFF', mouth: '#8A3D3A' });
  const BODY = 'M46 120a54 54 0 1 0 108 0a54 54 0 1 0-108 0Z';
  const MASK = 'M62 108Q80 97 100 112Q120 97 138 108Q136 131 100 123Q64 131 62 108Z';
  const EY = 114, EX = [80, 120];

  function eye(x, y, p, i, iris) {
    const ink = COL.ink, side = i ? 1 : -1, lx = Math.max(-2.4, Math.min(2.4, p.lookX * .5)), ly = Math.max(-2.6, Math.min(2.6, p.lookY * .45));
    const T = (c, s) => `<g transform="translate(${n(x + p.lookX * .3)} ${n(y + p.lookY * .35)}) scale(${s || .82})">${c}</g>`;
    if (p.eye === 'happy') return T(`<path d="M-10 3Q0 -9 10 3" fill="none" stroke="${ink}" stroke-width="3.8" stroke-linecap="round"/>`);
    if (p.eye === 'closed') return T(`<path d="M-11 0Q0 6 11 0" fill="none" stroke="${ink}" stroke-width="3.4" stroke-linecap="round"/><path d="M${-11 * side} 0l${-3 * side} -2" stroke="${ink}" stroke-width="2.4" stroke-linecap="round"/>`);
    const wide = p.eye === 'wide', sad = p.eye === 'sad';
    const bl = Math.max(.05, Math.min(1.15, p.eyeOpen)) * (sad ? .72 : 1);
    if (bl < .22) return T(`<path d="M-11 0Q0 5 11 0" fill="none" stroke="${ink}" stroke-width="3.2" stroke-linecap="round"/>`);
    const g = [], oy = sad ? 3 : 0;
    g.push(`<ellipse cx="0" cy="${oy}" rx="11" ry="${n(13.5 * bl)}" fill="${COL.white}"/>`);
    g.push(`<ellipse cx="${n(lx)}" cy="${n(oy + 1.2 * bl + ly)}" rx="8.4" ry="${n(11.4 * bl)}" fill="${iris}"/>`);
    g.push(`<ellipse cx="${n(lx)}" cy="${n(oy - 5 * bl + ly)}" rx="8.4" ry="${n(5.6 * bl)}" fill="${ink}" opacity=".2"/>`);
    g.push(`<ellipse cx="${n(lx)}" cy="${n(oy + 1.5 * bl + ly)}" rx="${wide ? 3.2 : 4}" ry="${n((wide ? 4.4 : 5.5) * bl)}" fill="${ink}"/>`);
    g.push(`<circle cx="${n(lx - 3.5)}" cy="${n(oy - 5 * bl + ly)}" r="${wide ? 3.8 : 3.2}" fill="#fff"/>`);
    g.push(`<circle cx="${n(lx + 3.6)}" cy="${n(oy + 5 * bl + ly)}" r="1.6" fill="#fff" opacity=".9"/>`);
    g.push(`<path d="M-12 ${n(oy - 7 * bl)}Q-1 ${n(oy - 21 * bl)} 12.5 ${n(oy - 8.5 * bl)}" fill="none" stroke="${ink}" stroke-width="3.2" stroke-linecap="round"/>`);
    g.push(`<path d="M${12 * side} ${n(oy - 7 * bl)}l${3.5 * side} .5" stroke="${ink}" stroke-width="2.4" stroke-linecap="round"/>`);
    if (sad) g.push(`<path d="M${-12 * side} -16L${10 * side} -12" stroke="${ink}" stroke-width="3" stroke-linecap="round"/>`);
    return T(g.join(''), wide ? .9 : .82);
  }
  function eyeMini(x, y, p, iris) {
    const ink = COL.ink, xx = n(x + p.lookX * .3), yy = n(y + p.lookY * .3);
    if (p.eye === 'happy') return `<path d="M${xx - 8} ${yy + 3}Q${xx} ${yy - 7} ${xx + 8} ${yy + 3}" fill="none" stroke="${ink}" stroke-width="4.6" stroke-linecap="round"/>`;
    const bl = Math.max(.05, Math.min(1.15, p.eyeOpen)) * (p.eye === 'sad' ? .6 : 1);
    if (p.eye === 'closed' || bl < .22) return `<path d="M${xx - 8} ${yy}Q${xx} ${yy + 5} ${xx + 8} ${yy}" fill="none" stroke="${ink}" stroke-width="4.2" stroke-linecap="round"/>`;
    return `<ellipse cx="${xx}" cy="${yy}" rx="8.5" ry="${n(10.5 * bl)}" fill="${COL.white}"/><ellipse cx="${xx}" cy="${n(yy + 1)}" rx="6" ry="${n(8 * bl)}" fill="${iris === COL.iris ? ink : iris}"/><circle cx="${n(xx - 2.4)}" cy="${n(yy - 3.6 * bl)}" r="2.6" fill="#fff"/>`;
  }
  function mouth(p, ms) {
    const ink = COL.ink;
    let m = 'cat';
    if (p.eye === 'happy') m = 'open'; else if (p.eye === 'wide') m = 'o'; else if (p.eye === 'sad') m = 'frown'; else if (p.eye === 'closed') m = 'flat';
    if (p.talk && Math.floor(ms / 190) % 3 === 1) m = 'o';
    const d = {
      cat: `<path d="M-6.3 -1q3.15 4 6.3 0q3.15 4 6.3 0" fill="none" stroke="${ink}" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round"/>`,
      o: `<ellipse cx="0" cy="1.5" rx="3" ry="3.6" fill="${COL.mouth}" stroke="${ink}" stroke-width="1.4"/>`,
      open: `<path d="M-6 -2Q0 9 6 -2Z" fill="${COL.mouth}" stroke="${ink}" stroke-width="1.6" stroke-linejoin="round"/><path d="M-3 3Q0 5 3 3" stroke="${COL.coral}" stroke-width="2" stroke-linecap="round"/>`,
      frown: `<path d="M-5 3Q0 -2 5 3" fill="none" stroke="${ink}" stroke-width="2.1" stroke-linecap="round"/>`,
      flat: `<path d="M-3.5 1h7" stroke="${ink}" stroke-width="2.1" stroke-linecap="round"/>`
    }[m];
    return `<g transform="translate(100 130)">${d}</g>`;
  }
  const ear = (x, a, m, body, rim, mini) => `<g transform="translate(${x} 90) rotate(${n(a)}) scale(${m} 1)"><circle cy="-16" r="15" fill="${body}" ${mini ? '' : `stroke="${rim}" stroke-width="2"`}/><circle cx="1" cy="-16" r="8" fill="${COL.coral}" opacity="${mini ? 1 : .85}"/></g>`;

  function figure(state, p, ms, mini) {
    const g = [], body = mini ? COL.mini : COL.body, rim = COL.rim, st = mini ? '' : `stroke="${rim}" stroke-width="2" stroke-linejoin="round"`;
    const iris = mix(COL.iris, COL.cyan, p.glow);
    if (!mini) g.push(`<g transform="translate(146 156) rotate(${n(-p.tail)})"><path d="M0 0C20 4 32 -12 28 -30" fill="none" stroke="${rim}" stroke-width="18" stroke-linecap="round"/><path d="M0 0C20 4 32 -12 28 -30" fill="none" stroke="${body}" stroke-width="14" stroke-linecap="round"/><path d="M14 0l4 -10M24 -10l8 -4M28 -22l8 0" stroke="${COL.mask}" stroke-width="4" stroke-linecap="round"/></g>`);
    g.push(ear(74, -p.earL, 1, body, rim, mini), ear(126, p.earR, -1, body, rim, mini));
    g.push(`<path d="${BODY}" fill="${body}" ${st}/>`);
    g.push(`<ellipse cx="100" cy="152" rx="${mini ? 30 : 34}" ry="20" fill="${COL.belly}"/>`);
    if (!mini) g.push(`<path d="M70 104C72 84 84 74 96 72" fill="none" stroke="#fff" stroke-opacity=".12" stroke-width="7" stroke-linecap="round"/>`);
    g.push(`<path d="${MASK}" fill="${COL.mask}"/>`);
    if (p.glow > .01 && !mini) g.push(`<ellipse cx="100" cy="${EY}" rx="44" ry="20" fill="${COL.cyan}" opacity="${n(.18 * p.glow)}"/>`);
    EX.forEach((x, i) => g.push(mini ? eyeMini(x, EY, p, iris) : eye(x, EY, p, i, iris)));
    if (!mini) {
      g.push(`<ellipse cx="66" cy="129" rx="8" ry="4" fill="${COL.coral}" opacity="${n(p.eye === 'happy' ? .6 : .38)}"/><ellipse cx="134" cy="129" rx="8" ry="4" fill="${COL.coral}" opacity="${n(p.eye === 'happy' ? .6 : .38)}"/>`);
      g.push(`<ellipse cx="100" cy="124" rx="4.2" ry="3" fill="${COL.ink}"/>`);
      g.push(mouth(p, ms));
      g.push(`<ellipse cx="82" cy="170" rx="11" ry="6" fill="${COL.paw}" ${st}/>`);
      const px = 118 + (136 - 118) * p.pawR, py = 170 + (128 - 170) * p.pawR;
      g.push(`<ellipse cx="${n(px)}" cy="${n(py)}" rx="11" ry="6" transform="rotate(${n(-35 * p.pawR)} ${n(px)} ${n(py)})" fill="${COL.paw}" ${st}/>`);
    } else g.push(`<ellipse cx="100" cy="125" rx="5" ry="3.6" fill="${COL.ink}"/>`);
    return g.join('');
  }

  function template(p, size, color) {
    const tr = `translate(100 172) rotate(${n(p.rot * .5)}) scale(${n(p.sx)} ${n(p.sy)}) translate(-100 -172)`;
    const cy = EY + p.lookY * .4;
    let inner = MASK;
    EX.forEach(x0 => {
      const x = x0 + p.lookX * .4;
      if (p.eye === 'closed' || p.eye === 'happy' || p.eyeOpen < .25) inner += rr(x - 9, cy - 2, 18, 5, 2.5);
      else { const h = Math.max(6, 18 * Math.min(1, p.eyeOpen)); inner += rr(x - 7.5, cy - h / 2, 15, h, 7.5); }
    });
    const ears = [[74, -p.earL, 1], [126, p.earR, -1]].map(([x, a, m]) => `<circle transform="translate(${x} 90) rotate(${n(a)}) scale(${m} 1)" cy="-16" r="15" fill="${color}"/>`).join('');
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="36 46 128 130" style="display:block"><g transform="${tr}">${ears}<path d="${BODY}${inner}" fill="${color}" fill-rule="evenodd"/></g></svg>`;
  }

  function svg(state, t, opts) {
    opts = opts || {};
    const s = STATES[state] || STATES.idle;
    const u = ((t % 1) + 1) % 1, ms = u * s.dur;
    const p = pose(state, u);
    p.talk = state === 'streaming';
    const detail = opts.detail || 'full', mini = detail === 'mini', size = opts.size || 160;
    if (detail === 'template') return template(p, size, opts.color || '#000');
    const yk = mini ? .45 : 1;
    const tr = `translate(${n(100 + p.x * yk)} ${n(172 + p.y * yk)}) rotate(${n(p.rot)}) scale(${n(p.sx)} ${n(p.sy)}) translate(-100 -172)`;
    const lift = Math.max(0, -p.y);
    const sh = mini ? '' : `<ellipse cx="${n(100 + p.x)}" cy="176" rx="${n(48 * p.sx * (1 - lift / 70))}" ry="5" fill="#000" opacity="${n(.32 * (1 - lift / 50))}"/>`;
    const vb = mini ? '32 42 136 136' : '0 0 200 200';
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="${vb}" style="display:block;overflow:visible">${sh}<g transform="${tr}" opacity="${n(1 - p.dim * .35)}">${figure(state, p, ms, mini)}</g>${mini || opts.noProps ? '' : EC.props(state, p, ms)}</svg>`;
  }

  function spec(state) {
    const b = EC.spec(state);
    b.character = 'tanuki';
    b.pivot = { body: [100, 172], earL: [74, 90], earR: [126, 90], tail: [146, 156], pawR: [118, 170] };
    b.parts = ['shadow', 'tail', 'earL', 'earR', 'body', 'belly', 'mask', 'eyeL', 'eyeR', 'blush', 'nose', 'mouth', 'pawL', 'pawR', 'props'];
    b.palette = { body: COL.body, rim: COL.rim, mask: COL.mask, belly: COL.belly, iris: COL.iris, earInner: COL.coral, effect: COL.cyan };
    return b;
  }

  window.ElisaTanuki = { STATES, ORDER, EASE, EASE_INFO, COL, pose, svg, spec };
})();
