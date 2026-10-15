// Speech to text, two ways:
// - Whisper: the samples are collected in the page, turned into 16 kHz mono 16-bit PCM, and sent
//   to codeit, which runs Whisper on this machine.
// - The browser's own speech service (the Web Speech API): Edge sends the audio to Microsoft,
//   Chrome to Google. Nothing to install, but the audio leaves the machine.

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

export type Engine = "whisper" | "browser";

export interface Recording {
  /** Stops and gives what was said. */
  stop(): Promise<string>;
  cancel(): void;
  /** Loudness of the last moment, 0 to 1, for the meter. */
  level(): number;
}

export interface Listener {
  /** The browser's guess so far, while you talk. */
  partial(text: string): void;
  /** The browser's service stopped with an error while listening. */
  broke(e: Error): void;
}

/** Starts listening with an engine. */
export function listen(engine: Engine, language: string, on: Listener): Promise<Recording> {
  return engine === "browser" ? speech(language, on) : whisper();
}

async function whisper(): Promise<Recording> {
  const rec = await record();
  return { ...rec, stop: async () => transcribe(await rec.stop()) };
}

export interface Pcm {
  /** Stops and gives the audio as 16 kHz 16-bit PCM. */
  stop(): Promise<Int16Array>;
  cancel(): void;
  level(): number;
}

export async function record(): Promise<Pcm> {
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

// ── the browser's speech service ─────────────────────────────────────────────

interface SpeechResult {
  isFinal: boolean;
  0: { transcript: string };
}
interface SpeechEvent {
  resultIndex: number;
  results: ArrayLike<SpeechResult>;
}
interface Recognizer {
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  onresult: ((e: SpeechEvent) => void) | null;
  onerror: ((e: { error: string }) => void) | null;
  onend: (() => void) | null;
  onstart: (() => void) | null;
  onspeechstart: (() => void) | null;
  onspeechend: (() => void) | null;
  start(): void;
  stop(): void;
  abort(): void;
}
type RecognizerClass = new () => Recognizer;

function recognizer(): RecognizerClass | null {
  const w = globalThis as unknown as { SpeechRecognition?: RecognizerClass; webkitSpeechRecognition?: RecognizerClass };
  return w.SpeechRecognition ?? w.webkitSpeechRecognition ?? null;
}

/** Whether this browser has a speech service, and whose. */
export function browserSpeech(): { ok: boolean; who: string } {
  const ua = typeof navigator === "undefined" ? "" : navigator.userAgent;
  const who = /Edg\//.test(ua) ? "Microsoft (Edge)" : /Chrome\//.test(ua) ? "Google (Chrome)" : "the browser's provider";
  return { ok: recognizer() !== null, who };
}

const LOCALES: Record<string, string> = {
  en: "en-US",
  pt: "pt-BR",
  es: "es-ES",
  fr: "fr-FR",
  de: "de-DE",
  it: "it-IT",
};

/** The language codeit's settings use (`auto`, `en`, `pt`…) as the locale the service wants. */
export function locale(language: string, fallback: string): string {
  if (!language || language === "auto") return fallback || "en-US";
  return LOCALES[language] ?? language;
}

const WHY: Record<string, string> = {
  "not-allowed": "the microphone isn't allowed for this page",
  "service-not-allowed": "the browser's speech service is turned off or blocked",
  "audio-capture": "no microphone found",
  network: "couldn't reach the browser's speech service (network)",
  "language-not-supported": "the speech service doesn't know this language",
};

/** What was heard: the final parts, then the guess for the part still being said. */
export function heard(results: ArrayLike<SpeechResult>): { done: string; guess: string } {
  let done = "";
  let guess = "";
  for (let i = 0; i < results.length; i++) {
    const r = results[i];
    if (r.isFinal) done += r[0].transcript;
    else guess += r[0].transcript;
  }
  return { done, guess };
}

function speech(language: string, on: Listener): Promise<Recording> {
  const Rec = recognizer();
  if (!Rec) return Promise.reject(new Error("this browser has no speech service; use Whisper (Voice settings)"));
  const lang = locale(language, navigator.language);
  // Text from earlier runs: the service ends a run after a silence, and we start another.
  let kept = "";
  let current = { done: "", guess: "" };
  let talking = false;
  let started = false;
  let stopping = false;
  let failed: Error | null = null;
  let finish: ((text: string) => void) | null = null;
  let fail: ((e: Error) => void) | null = null;
  let rec: Recognizer;

  const text = () => joined(kept, current.done, current.guess);

  const run = (first: (() => void) | null, firstFail: ((e: Error) => void) | null) => {
    rec = new Rec();
    rec.lang = lang;
    rec.continuous = true;
    rec.interimResults = true;
    rec.onstart = () => {
      started = true;
      first?.();
    };
    rec.onspeechstart = () => (talking = true);
    rec.onspeechend = () => (talking = false);
    rec.onresult = (e) => {
      current = heard(e.results);
      on.partial(text());
    };
    rec.onerror = (e) => {
      if (e.error === "no-speech" || e.error === "aborted") return;
      failed = new Error(`Speech: ${WHY[e.error] ?? e.error}`);
      firstFail?.(failed);
    };
    rec.onend = () => {
      kept = joined(kept, current.done, current.guess);
      current = { done: "", guess: "" };
      talking = false;
      if (failed) {
        if (fail) fail(failed);
        else if (started) on.broke(failed);
      } else if (stopping) finish?.(kept.trim());
      else run(null, null);
    };
    rec.start();
  };

  return new Promise<Recording>((resolve, reject) => {
    const recording: Recording = {
      level: () => (talking ? 0.6 : 0.1),
      cancel: () => {
        stopping = true;
        finish = null;
        fail = null;
        rec.abort();
      },
      stop: () =>
        new Promise<string>((res, rej) => {
          stopping = true;
          finish = res;
          fail = rej;
          if (failed) rej(failed);
          else rec.stop();
        }),
    };
    run(
      () => resolve(recording),
      (e) => reject(e),
    );
  });
}

function joined(...parts: string[]): string {
  return parts
    .map((p) => p.trim())
    .filter(Boolean)
    .join(" ");
}
