/// <reference lib="webworker" />
// Off-main-thread video paint. The pump used to do `new Uint8ClampedArray(buf)` +
// `putImageData` on the renderer's main thread every frame — two ~8MB CPU ops per frame
// that, on Windows, dragged the framerate down. This worker takes ownership of the
// player's <canvas> (OffscreenCanvas) and paints frames here instead:
//   - WebGL path (preferred): upload the RGBA frame as a GPU texture and draw a quad —
//     the copy/scale happens on the GPU, off the CPU entirely.
//   - 2D fallback: if WebGL can't init, putImageData on the worker thread — still off the
//     main thread, guaranteed correct (no black screen).
// The main thread only receives the frame over IPC and transfers it here (zero-copy).

type Msg = { canvas: OffscreenCanvas } | { buffer: ArrayBuffer; w: number; h: number };

let canvas: OffscreenCanvas | null = null;
let gl: WebGLRenderingContext | null = null;
let ctx2d: OffscreenCanvasRenderingContext2D | null = null;
let tex: WebGLTexture | null = null;
let texW = 0;
let texH = 0;

function initWebGL(c: OffscreenCanvas): boolean {
  const g = (c.getContext("webgl2") || c.getContext("webgl")) as WebGLRenderingContext | null;
  if (!g) return false;
  const vs = `attribute vec2 p; varying vec2 uv;
    void main(){ uv = vec2((p.x + 1.0) * 0.5, 1.0 - (p.y + 1.0) * 0.5); gl_Position = vec4(p, 0.0, 1.0); }`;
  const fs = `precision mediump float; varying vec2 uv; uniform sampler2D t;
    void main(){ gl_FragColor = texture2D(t, uv); }`;
  const sh = (type: number, src: string) => {
    const s = g.createShader(type)!;
    g.shaderSource(s, src);
    g.compileShader(s);
    return s;
  };
  const prog = g.createProgram()!;
  g.attachShader(prog, sh(g.VERTEX_SHADER, vs));
  g.attachShader(prog, sh(g.FRAGMENT_SHADER, fs));
  g.linkProgram(prog);
  if (!g.getProgramParameter(prog, g.LINK_STATUS)) return false;
  g.useProgram(prog);
  const buf = g.createBuffer();
  g.bindBuffer(g.ARRAY_BUFFER, buf);
  // Two triangles covering the clip-space quad.
  g.bufferData(g.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]), g.STATIC_DRAW);
  const loc = g.getAttribLocation(prog, "p");
  g.enableVertexAttribArray(loc);
  g.vertexAttribPointer(loc, 2, g.FLOAT, false, 0, 0);
  tex = g.createTexture();
  g.bindTexture(g.TEXTURE_2D, tex);
  g.texParameteri(g.TEXTURE_2D, g.TEXTURE_MIN_FILTER, g.LINEAR);
  g.texParameteri(g.TEXTURE_2D, g.TEXTURE_MAG_FILTER, g.LINEAR);
  g.texParameteri(g.TEXTURE_2D, g.TEXTURE_WRAP_S, g.CLAMP_TO_EDGE);
  g.texParameteri(g.TEXTURE_2D, g.TEXTURE_WRAP_T, g.CLAMP_TO_EDGE);
  gl = g;
  return true;
}

function paint(data: Uint8Array, w: number, h: number) {
  if (!canvas) return;
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
    if (gl) gl.viewport(0, 0, w, h);
  }
  if (gl && tex) {
    gl.bindTexture(gl.TEXTURE_2D, tex);
    if (w !== texW || h !== texH) {
      texW = w;
      texH = h;
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, data);
    } else {
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, data);
    }
    gl.drawArrays(gl.TRIANGLES, 0, 6);
  } else if (ctx2d) {
    ctx2d.putImageData(new ImageData(new Uint8ClampedArray(data), w, h), 0, 0);
  }
}

self.onmessage = (e: MessageEvent<Msg>) => {
  const m = e.data;
  if ("canvas" in m) {
    canvas = m.canvas;
    if (!initWebGL(canvas)) {
      ctx2d = canvas.getContext("2d");
    }
  } else if ("buffer" in m) {
    paint(new Uint8Array(m.buffer), m.w, m.h);
  }
};
