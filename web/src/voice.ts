// Recording the microphone for speech to text: the samples are collected in the page, turned
// into 16 kHz mono 16-bit PCM, and sent to codeit, which runs Whisper on this machine.

const RATE = 16_000;

// Copies the microphone's samples out of the audio thread.
const WORKLET = `
class Tap extends AudioWorkletProcessor {
  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (ch) this.port.postMessage(ch.slice(0));
    return true;
  }
}
registerProcessor("codeit-tap", Tap);
`;

export interface Recording {
  /** Stops and gives the audio as 16 kHz 16-bit PCM. */
  stop(): Promise<Int16Array>;
  cancel(): void;
  /** Loudness of the last moment, 0 to 1, for the meter. */
  level(): number;
}

export async function record(): Promise<Recording> {
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new Error("This browser can't record here (open codeit at http://localhost).");
  }
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true },
  });
  const ctx = new AudioContext();
  const url = URL.createObjectURL(new Blob([WORKLET], { type: "text/javascript" }));
  await ctx.audioWorklet.addModule(url);
  URL.revokeObjectURL(url);
  const source = ctx.createMediaStreamSource(stream);
  const tap = new AudioWorkletNode(ctx, "codeit-tap");
  const chunks: Float32Array[] = [];
  let last = 0;
  tap.port.onmessage = (e: MessageEvent<Float32Array>) => {
    chunks.push(e.data);
    let sum = 0;
    for (const v of e.data) sum += v * v;
    last = Math.min(1, Math.sqrt(sum / e.data.length) * 4);
  };
  source.connect(tap);
  // A worklet only runs when connected to the output; it writes nothing there.
  tap.connect(ctx.destination);

  const close = () => {
    source.disconnect();
    tap.disconnect();
    for (const t of stream.getTracks()) t.stop();
    void ctx.close();
  };

  return {
    level: () => last,
    cancel: close,
    async stop() {
      const rate = ctx.sampleRate;
      close();
      return toPcm16k(join(chunks), rate);
    },
  };
}

function join(chunks: Float32Array[]): Float32Array {
  const out = new Float32Array(chunks.reduce((n, c) => n + c.length, 0));
  let at = 0;
  for (const c of chunks) {
    out.set(c, at);
    at += c.length;
  }
  return out;
}

/** Resamples to 16 kHz (the browser's own resampler) and converts to 16-bit samples. */
export async function toPcm16k(samples: Float32Array, rate: number): Promise<Int16Array> {
  if (samples.length === 0) return new Int16Array(0);
  let mono = samples;
  if (rate !== RATE) {
    const frames = Math.ceil((samples.length * RATE) / rate);
    const offline = new OfflineAudioContext(1, frames, RATE);
    const buf = offline.createBuffer(1, samples.length, rate);
    buf.copyToChannel(new Float32Array(samples), 0);
    const src = offline.createBufferSource();
    src.buffer = buf;
    src.connect(offline.destination);
    src.start();
    mono = (await offline.startRendering()).getChannelData(0);
  }
  return floatTo16(mono);
}

export function floatTo16(samples: Float32Array): Int16Array {
  const out = new Int16Array(samples.length);
  for (let i = 0; i < samples.length; i++) {
    const s = Math.max(-1, Math.min(1, samples[i]));
    out[i] = s < 0 ? s * 0x8000 : s * 0x7fff;
  }
  return out;
}

/** Sends the audio to codeit and gives back the text. */
export async function transcribe(pcm: Int16Array): Promise<string> {
  const res = await fetch("/api/voice", {
    method: "POST",
    headers: { "Content-Type": "application/octet-stream" },
    body: new Blob([pcm.slice().buffer]),
  });
  const body = (await res.json().catch(() => ({}))) as { text?: string; error?: string };
  if (!res.ok) throw new Error(body.error ?? `speech to text failed (HTTP ${res.status})`);
  return body.text ?? "";
}
