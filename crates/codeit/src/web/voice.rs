//! Speech to text, on this machine. The browser records the microphone and sends 16 kHz mono
//! samples; Whisper turns them into text, either built in (the `voice` feature: whisper.cpp
//! through whisper-rs) or through a `whisper-cli` on the PATH (whisper.cpp's own program).
//!
//! The model (a ggml file) is downloaded once to `~/.local/share/codeit/whisper/`, or taken from
//! `CODEIT_WHISPER_MODEL` (a path) where Hugging Face can't be reached.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};

/// The models to choose from: (name, size in MB, what it is).
pub const MODELS: &[(&str, u64, &str)] = &[
    ("tiny.en", 75, "English, fastest"),
    ("base.en", 142, "English, fast and accurate (default)"),
    ("small.en", 466, "English, most accurate, slower"),
    ("tiny", 75, "any language, fastest"),
    ("base", 142, "any language"),
    ("small", 466, "any language, most accurate, slower"),
];

pub const DEFAULT_MODEL: &str = "base.en";

/// Samples per second Whisper expects.
pub const RATE: usize = 16_000;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Engine {
    /// whisper.cpp built into this binary.
    Builtin,
    /// whisper.cpp's `whisper-cli` (or the older `whisper-cpp`), found on the PATH.
    Cli(&'static str),
}

impl Engine {
    pub fn find() -> Option<Engine> {
        if cfg!(feature = "voice") {
            return Some(Engine::Builtin);
        }
        ["whisper-cli", "whisper-cpp"].into_iter().find(|p| on_path(p)).map(Engine::Cli)
    }

    fn label(self) -> &'static str {
        match self {
            Engine::Builtin => "built in",
            Engine::Cli(p) => p,
        }
    }
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
}

fn dir() -> PathBuf {
    codeit_providers::data_dir().join("whisper")
}

/// Where a model's file is (or will be once downloaded).
pub fn model_path(name: &str) -> PathBuf {
    match std::env::var_os("CODEIT_WHISPER_MODEL") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => dir().join(format!("ggml-{name}.bin")),
    }
}

fn model_url(name: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{name}.bin")
}

#[derive(Default)]
struct State {
    /// (bytes done, bytes in all) while a model downloads.
    downloading: Option<(String, u64, Option<u64>)>,
    error: Option<String>,
    #[cfg(feature = "voice")]
    loaded: Option<(PathBuf, Arc<whisper_rs::WhisperContext>)>,
}

pub struct Voice {
    pub engine: Option<Engine>,
    state: Mutex<State>,
    /// Tells every open tab how the voice setup is doing.
    notify: Box<dyn Fn(Value) + Send + Sync>,
}

impl Voice {
    pub fn new(notify: impl Fn(Value) + Send + Sync + 'static) -> Voice {
        #[cfg(feature = "voice")]
        whisper_rs::install_logging_hooks();
        Voice { engine: Engine::find(), state: Mutex::default(), notify: Box::new(notify) }
    }

    /// What the app shows: whether voice works here, the models, and which are downloaded.
    pub fn describe(&self, model: &str, language: &str) -> Value {
        let s = self.state.lock().unwrap();
        let custom = std::env::var_os("CODEIT_WHISPER_MODEL").is_some_and(|p| !p.is_empty());
        json!({
            "engine": self.engine.map(Engine::label),
            "model": model,
            "language": language,
            "custom_model": custom,
            "models": MODELS.iter().map(|(name, mb, what)| json!({
                "name": name,
                "mb": mb,
                "what": what,
                "ready": model_path(name).is_file(),
            })).collect::<Vec<_>>(),
            "downloading": s.downloading.as_ref().map(|(name, done, all)| json!({"model": name, "done": done, "total": all})),
            "error": s.error,
        })
    }

    fn update(&self, model: &str, language: &str) {
        (self.notify)(self.describe(model, language));
    }

    /// Downloads the model unless it is already here.
    pub async fn ensure(&self, model: &str, language: &str) -> Result<PathBuf> {
        let path = model_path(model);
        if path.is_file() {
            return Ok(path);
        }
        if !MODELS.iter().any(|(n, ..)| *n == model) {
            bail!("unknown Whisper model `{model}`");
        }
        {
            let mut s = self.state.lock().unwrap();
            if s.downloading.is_some() {
                bail!("a model is already downloading");
            }
            s.downloading = Some((model.to_string(), 0, None));
            s.error = None;
        }
        self.update(model, language);
        let result = self.download(model, language, &path).await;
        {
            let mut s = self.state.lock().unwrap();
            s.downloading = None;
            s.error = result.as_ref().err().map(|e| format!("{e:#}"));
        }
        self.update(model, language);
        result.map(|_| path)
    }

    async fn download(&self, model: &str, language: &str, path: &PathBuf) -> Result<()> {
        std::fs::create_dir_all(dir())?;
        let url = model_url(model);
        let resp = reqwest::get(&url).await.with_context(|| format!("downloading {url}"))?;
        if !resp.status().is_success() {
            bail!("downloading {url}: HTTP {}", resp.status());
        }
        let total = resp.content_length();
        let part = path.with_extension("part");
        let mut file = tokio::fs::File::create(&part).await?;
        let mut done = 0u64;
        let mut last = 0u64;
        let mut body = resp.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.context("downloading the model")?;
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
            done += chunk.len() as u64;
            if done - last > 2_000_000 {
                last = done;
                self.state.lock().unwrap().downloading = Some((model.to_string(), done, total));
                self.update(model, language);
            }
        }
        tokio::io::AsyncWriteExt::flush(&mut file).await?;
        drop(file);
        if total.is_some_and(|t| t != done) {
            let _ = std::fs::remove_file(&part);
            bail!("the download stopped early");
        }
        std::fs::rename(&part, path)?;
        Ok(())
    }

    /// Turns 16 kHz mono samples into text.
    pub async fn transcribe(self: &Arc<Self>, samples: Vec<i16>, model: &str, language: &str) -> Result<String> {
        let engine = self.engine.ok_or_else(|| anyhow!("no speech-to-text engine: see README, Voice"))?;
        if samples.len() < RATE / 4 {
            return Ok(String::new());
        }
        let path = self.ensure(model, language).await?;
        // English-only models take no language.
        let lang = if model.ends_with(".en") || language.is_empty() { None } else { Some(language.to_string()) };
        match engine {
            Engine::Builtin => {
                let me = self.clone();
                tokio::task::spawn_blocking(move || me.builtin(&path, &samples, lang.as_deref())).await?
            }
            Engine::Cli(program) => cli(program, &path, &samples, lang.as_deref()).await,
        }
    }

    #[cfg(feature = "voice")]
    fn builtin(&self, path: &PathBuf, samples: &[i16], language: Option<&str>) -> Result<String> {
        use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
        let ctx = {
            let mut s = self.state.lock().unwrap();
            match &s.loaded {
                Some((p, ctx)) if p == path => ctx.clone(),
                _ => {
                    let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
                        .map_err(|e| anyhow!("loading {}: {e}", path.display()))?;
                    let ctx = Arc::new(ctx);
                    s.loaded = Some((path.clone(), ctx.clone()));
                    ctx
                }
            }
        };
        let mut state = ctx.create_state().map_err(|e| anyhow!("whisper: {e}"))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
        params.set_n_threads(threads as i32);
        params.set_language(Some(language.unwrap_or("auto")));
        params.set_no_context(true);
        params.set_no_timestamps(true);
        params.set_suppress_blank(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        let audio: Vec<f32> = samples.iter().map(|s| *s as f32 / 32768.0).collect();
        state.full(params, &audio).map_err(|e| anyhow!("whisper: {e}"))?;
        let mut text = String::new();
        for segment in state.as_iter() {
            text.push_str(&segment.to_str_lossy().map_err(|e| anyhow!("whisper: {e}"))?);
        }
        Ok(clean(&text))
    }

    #[cfg(not(feature = "voice"))]
    fn builtin(&self, _path: &PathBuf, _samples: &[i16], _language: Option<&str>) -> Result<String> {
        bail!("this codeit was built without the `voice` feature")
    }
}

/// A WAV file of 16-bit mono samples.
pub fn wav(samples: &[i16]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&(RATE as u32).to_le_bytes());
    out.extend_from_slice(&(RATE as u32 * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

async fn cli(program: &str, model: &PathBuf, samples: &[i16], language: Option<&str>) -> Result<String> {
    let file = std::env::temp_dir().join(format!("codeit-voice-{}.wav", uuid::Uuid::new_v4()));
    std::fs::write(&file, wav(samples))?;
    let out = tokio::process::Command::new(program)
        .arg("-m")
        .arg(model)
        .arg("-f")
        .arg(&file)
        .args(["-nt", "-np", "-l", language.unwrap_or("auto")])
        .output()
        .await;
    let _ = std::fs::remove_file(&file);
    let out = out.with_context(|| format!("running {program}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{program} failed: {}", err.trim().lines().last().unwrap_or_default());
    }
    Ok(clean(&String::from_utf8_lossy(&out.stdout)))
}

/// Joins the segments and drops what Whisper writes for silence or noise: `[BLANK_AUDIO]`
/// anywhere, and a reply that is only `(music)`.
fn clean(text: &str) -> String {
    let mut kept = String::new();
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '[' => depth += 1,
            ']' if depth > 0 => depth -= 1,
            _ if depth == 0 => kept.push(c),
            _ => {}
        }
    }
    let out = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.starts_with('(') && out.ends_with(')') && !out[1..].contains('(') { String::new() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_silence_tags() {
        assert_eq!(clean(" [BLANK_AUDIO]\n Fix the  test (the parser).\n"), "Fix the test (the parser).");
        assert_eq!(clean(" (upbeat music) "), "");
    }

    #[test]
    fn writes_a_wav_header() {
        let w = wav(&[0, 1, -1]);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 6);
    }
}
