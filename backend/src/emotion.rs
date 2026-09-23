//! 情绪识别：在本地跑一个多语言的 ONNX 分类模型。
//!
//! 模型是 distilbert-base-multilingual-cased 微调出来的三分类
//! （positive / neutral / negative），支持中文 —— 英文的 GoEmotions 细粒度
//! 模型对中文输入基本失效（分词器会把整句变成 [UNK]），所以这里选多语言版。
//!
//! 运行时依赖系统的 libonnxruntime（Debian 包 `libonnxruntime1.21`），
//! 二进制不内嵌模型，模型文件放在数据目录下的 models/emotion/。

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::OnceLock;

use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;

/// 低于这个置信度就按中性处理，避免弱信号去调语气。
/// 实测：真正的喜怒都很笃定（0.98 / 0.99），而「帮我看下内存用了多少」
/// 这类命令式句子会拿到 0.37~0.57 —— 0.65 能干净地分开这两类。
const NEUTRAL_THRESHOLD: f32 = 0.65;

/// 一次情绪识别的结果
#[derive(Serialize, Clone, Debug)]
pub struct Emotion {
    /// 三类标签之一：positive / neutral / negative
    pub label: String,
    /// 置信度 0~1
    pub score: f32,
    /// 折成效价：正 +1、中 0、负 -1，再乘置信度
    pub valence: f32,
}

/// 模型目录下的文件是否齐全
pub fn model_dir(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("models/emotion")
}

/// 模型是否已就绪（文件齐全），供前端显示状态
pub fn ready(data_dir: &str) -> bool {
    let d = model_dir(data_dir);
    d.join("model_quantized.onnx").is_file() && d.join("tokenizer.json").is_file()
}

/// 推理器：模型只加载一次，之后复用
struct Engine {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    labels: Vec<String>,
}

static ENGINE: OnceLock<Engine> = OnceLock::new();

/// 让 ort 知道去哪加载 ONNX Runtime。
///
/// 用 load-dynamic 时运行库不在二进制里，得指个路径；优先尊重环境变量，
/// 否则按 Debian 常见位置找。找不到就让 ort 报错，别静默失败。
fn init_runtime() -> Result<()> {
    static INITED: OnceLock<Result<(), String>> = OnceLock::new();
    let res = INITED.get_or_init(|| {
        let candidates = [
            std::env::var("ORT_DYLIB_PATH").unwrap_or_default(),
            "/usr/lib/x86_64-linux-gnu/libonnxruntime.so".to_string(),
            "/usr/lib/libonnxruntime.so".to_string(),
            "/usr/local/lib/libonnxruntime.so".to_string(),
        ];
        for p in candidates.iter().filter(|p| !p.is_empty()) {
            if Path::new(p).is_file() {
                return ort::init_from(p)
                    .map(|_| ())
                    .map_err(|e| format!("加载 ONNX Runtime（{p}）失败：{e}"));
            }
        }
        Err("找不到 ONNX Runtime 运行库，请安装 libonnxruntime1.21".to_string())
    });
    match res {
        Ok(()) => Ok(()),
        Err(e) => bail!("{e}"),
    }
}

/// 读取 config.json 里的 id2label，拿不到就按三分类的默认顺序
fn load_labels(dir: &Path) -> Vec<String> {
    let fallback = || vec!["positive".into(), "neutral".into(), "negative".into()];
    let Ok(text) = std::fs::read_to_string(dir.join("config.json")) else {
        return fallback();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return fallback();
    };
    let Some(map) = v.get("id2label").and_then(|m| m.as_object()) else {
        return fallback();
    };
    let mut pairs: Vec<(i64, String)> = map
        .iter()
        .filter_map(|(k, val)| Some((k.parse::<i64>().ok()?, val.as_str()?.to_string())))
        .collect();
    if pairs.is_empty() {
        return fallback();
    }
    pairs.sort_by_key(|(i, _)| *i);
    pairs.into_iter().map(|(_, l)| l).collect()
}

/// 首次调用时把模型加载好
fn engine(data_dir: &str) -> Result<&'static Engine> {
    if let Some(e) = ENGINE.get() {
        return Ok(e);
    }
    init_runtime()?;
    let dir = model_dir(data_dir);
    if !ready(data_dir) {
        bail!(
            "情绪模型还没装好（缺 {} 下的 model_quantized.onnx 或 tokenizer.json）",
            dir.display()
        );
    }
    let session = Session::builder()
        .context("创建 ONNX 会话失败")?
        .commit_from_file(dir.join("model_quantized.onnx"))
        .context("加载情绪模型失败")?;
    let tokenizer =
        Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow::anyhow!("加载分词器失败：{e}"))?;
    let labels = load_labels(&dir);
    let engine = Engine {
        session: Mutex::new(session),
        tokenizer,
        labels,
    };
    // 竞态下可能已有别的线程放进去，get_or_init 的返回值以先到者为准
    let _ = ENGINE.set(engine);
    ENGINE.get().context("情绪模型初始化失败")
}

/// 识别一段文本的情绪。模型推理是同步的，调用方应放在阻塞线程里跑。
pub fn classify(data_dir: &str, text: &str) -> Result<Emotion> {
    let text = text.trim();
    if text.is_empty() {
        bail!("文本为空");
    }
    let e = engine(data_dir)?;

    let enc = e
        .tokenizer
        .encode(text, true)
        .map_err(|err| anyhow::anyhow!("分词失败：{err}"))?;
    // 模型的位置编码上限是 512
    let ids: Vec<i64> = enc.get_ids().iter().take(512).map(|v| *v as i64).collect();
    let mask: Vec<i64> = enc
        .get_attention_mask()
        .iter()
        .take(512)
        .map(|v| *v as i64)
        .collect();
    let len = ids.len();

    let mut session = e.session.lock().unwrap_or_else(|p| p.into_inner());
    let outputs = session
        .run(ort::inputs![
            "input_ids" => Tensor::from_array((vec![1i64, len as i64], ids))?,
            "attention_mask" => Tensor::from_array((vec![1i64, len as i64], mask))?,
        ])
        .context("情绪模型推理失败")?;
    let (_, logits) = outputs[0]
        .try_extract_tensor::<f32>()
        .context("读取模型输出失败")?;

    // 三分类做 softmax
    let max = logits.iter().cloned().fold(f32::MIN, f32::max);
    let exps: Vec<f32> = logits.iter().map(|v| (v - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    let probs: Vec<f32> = exps.iter().map(|v| v / sum).collect();
    let (idx, score) = probs
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(i, s)| (i, *s))
        .unwrap_or((1, 0.0));

    // 置信度不够就别去调语气：这个蒸馏小模型在中性的陈述句上常给出
    // 0.3~0.5 的「正面」分（实测「帮我看下内存用了多少」→ positive 0.37），
    // 照它调音量会让语气莫名其妙地欢快起来。低于阈值一律当中性。
    let label = if score < NEUTRAL_THRESHOLD {
        "neutral".to_string()
    } else {
        e.labels
            .get(idx)
            .cloned()
            .unwrap_or_else(|| "neutral".to_string())
    };
    let sign = match label.as_str() {
        "positive" => 1.0,
        "negative" => -1.0,
        _ => 0.0,
    };
    Ok(Emotion {
        label,
        score,
        valence: sign * score,
    })
}
