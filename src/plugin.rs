//! 插件模式：作为 rmusd 的子进程运行（`--rmus-plugin`），stdin/stdout
//! 走 NDJSON 协议；`--rmus-manifest` 则输出一行能力宣告后退出。
//!
//! 协议约定见 rmus 仓库 `src/plugin.rs` 的文档。本实现的三个方法：
//! - `search`（短调用）：并行查酷我/酷狗，返回 `{"songs":[...]}`
//! - `download`（长任务）：解析直链 → 下载 → 元数据门禁 → 落盘库目录；
//!   期间发 `progress` 通知，结束回 `{"path":...}`（宿主包装成 done 事件）
//! - `cancel`（短调用）：置取消开关，被取消的任务自己回 error
//!
//! 约束：stdout 只输出协议行（进度/日志全部走 stderr），这是宿主解析
//! 会话的硬前提。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};

use crate::download::{DownloadOptions, download_file};
use crate::sources::{MusicSource, SearchResult, kugou::Kugou, kuwo::Kuwo, write_metadata};

/// 通知行：宿主转发给所有客户端（`Event::Plugin`）。
#[derive(Serialize)]
struct Notify<'a> {
    event: &'a str,
    call: u64,
    data: serde_json::Value,
}

/// 宿主发来的请求行。
#[derive(Deserialize)]
struct InMsg {
    id: u64,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

/// 搜索结果对外的形状（与 rmus TUI 的 OnlineSong 对齐）。
#[derive(Debug, Serialize)]
struct SongOut {
    title: String,
    artist: String,
    album: String,
    source: String,
    song_id: String,
}

impl From<&SearchResult> for SongOut {
    fn from(r: &SearchResult) -> Self {
        Self {
            title: r.title.clone(),
            artist: r.artist.clone().unwrap_or_default(),
            album: r.album.clone().unwrap_or_default(),
            source: r.source.to_string(),
            song_id: r.song_id.clone(),
        }
    }
}

/// 客户端回传的 song 对象（下载时的寻址凭据）。
#[derive(Debug, Deserialize)]
struct SongRef {
    title: String,
    #[serde(default)]
    artist: String,
    source: String,
    song_id: String,
}

impl From<&SongRef> for SearchResult {
    fn from(s: &SongRef) -> Self {
        Self {
            title: s.title.clone(),
            artist: (!s.artist.is_empty()).then_some(s.artist.clone()),
            album: None,
            song_id: s.song_id.clone(),
            source: match s.source.as_str() {
                "kugou" => "kugou",
                _ => "kuwo",
            },
            cover_url: None,
        }
    }
}

/// 下载进度节流通知器：块级回调太密，150ms 聚合一次。
struct ProgressNotify {
    tx: mpsc::Sender<String>,
    call: u64,
    received: AtomicU64,
    last: Mutex<Instant>,
}

impl ProgressNotify {
    fn push(&self, chunk: u64, total: Option<u64>) {
        let received = self.received.fetch_add(chunk, Ordering::Relaxed) + chunk;
        let mut last = self.last.lock().unwrap();
        let finished = total.is_some_and(|t| received >= t);
        if !finished && last.elapsed() < Duration::from_millis(150) {
            return;
        }
        *last = Instant::now();
        let data = serde_json::json!({
            "received": received,
            "total": total.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
        });
        let line = serde_json::to_string(&Notify {
            event: "progress",
            call: self.call,
            data,
        })
        .unwrap_or_default();
        // 进度是可丢的：通道满就放弃这一条（150ms 节流，丢了无碍）。
        // 注意不能调 async 的 send：那会创建 future 被 here 丢弃，一条也发不出。
        let _ = self.tx.try_send(line);
    }
}

/// `--rmus-manifest` 的输出：能力宣告。
pub fn manifest_json() -> String {
    serde_json::json!({
        "name": "dl",
        "version": env!("CARGO_PKG_VERSION"),
        "methods": [
            {"name": "search", "kind": "call"},
            {"name": "download", "kind": "task"},
            {"name": "cancel", "kind": "call"},
        ],
    })
    .to_string()
}

/// 插件主循环：读一行、处理一行；下载任务在后台并发跑。
pub async fn run() -> anyhow::Result<()> {
    let kuwo = Kuwo::new().ok();
    let kugou = Kugou::new().ok();
    if kuwo.is_none() && kugou.is_none() {
        anyhow::bail!("没有任何可用音乐源（Kuwo/Kugou 初始化都失败了）");
    }
    let dest_dir = std::env::var_os("RMUS_LIBRARY_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("缺少 RMUS_LIBRARY_DIR（应由 rmusd 注入）"))?;

    // 协议行统一从一个通道写出去（主循环 + 下载任务共用），保证不串行交错
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                break;
            }
        }
    });

    // 在途下载：call -> 取消开关（cancel 方法翻开关，任务自己收尾回话）
    let jobs: Arc<Mutex<HashMap<u64, watch::Sender<bool>>>> = Arc::new(Mutex::new(HashMap::new()));

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(msg) = serde_json::from_str::<InMsg>(line.trim()) else {
            continue;
        };
        match msg.method.as_str() {
            "search" => {
                let query = msg
                    .params
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let reply = search(&kuwo, &kugou, &query).await;
                send_reply(&out_tx, msg.id, reply).await;
            }
            "download" => {
                let song = serde_json::from_value::<SongRef>(
                    msg.params.get("song").cloned().unwrap_or_default(),
                );
                let Ok(song) = song else {
                    send_reply(&out_tx, msg.id, Err(anyhow::anyhow!("song 参数不合法"))).await;
                    continue;
                };
                let (cancel_tx, cancel_rx) = watch::channel(false);
                jobs.lock().unwrap().insert(msg.id, cancel_tx);
                let out_tx = out_tx.clone();
                let jobs = jobs.clone();
                let dest_dir = dest_dir.clone();
                let kuwo = kuwo.clone();
                let kugou = kugou.clone();
                let id = msg.id;
                tokio::spawn(async move {
                    let result =
                        download_job(&kuwo, &kugou, &song, &dest_dir, id, &out_tx, cancel_rx).await;
                    jobs.lock().unwrap().remove(&id);
                    send_reply(&out_tx, id, result).await;
                });
            }
            "cancel" => {
                let call = msg.params.get("call").and_then(|v| v.as_u64());
                let cancelled = call
                    .and_then(|call| jobs.lock().unwrap().get(&call).map(|tx| tx.send(true).is_ok()))
                    .unwrap_or(false);
                send_reply(
                    &out_tx,
                    msg.id,
                    Ok(serde_json::json!({"cancelled": cancelled})),
                )
                .await;
            }
            other => {
                send_reply(
                    &out_tx,
                    msg.id,
                    Err(anyhow::anyhow!("未知方法 “{other}”")),
                )
                .await;
            }
        }
    }
    // stdin 关闭 = 宿主要我们走（或已死）；下载任务随进程退出
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}

/// 并行查酷我 + 酷狗；单源失败不挡另一源（错误拼进结果说明）。
async fn search(
    kuwo: &Option<Kuwo>,
    kugou: &Option<Kugou>,
    query: &str,
) -> anyhow::Result<serde_json::Value> {
    if query.is_empty() {
        anyhow::bail!("缺少 query 参数");
    }
    let mut notes = Vec::new();

    let kw = async {
        match kuwo {
            Some(k) => k.search(query).await,
            None => Err(anyhow::anyhow!("源不可用")),
        }
    };
    let kg = async {
        match kugou {
            Some(k) => k.search(query).await,
            None => Err(anyhow::anyhow!("源不可用")),
        }
    };
    let (kw, kg) = tokio::join!(kw, kg);

    let mut songs = Vec::new();
    match kw {
        Ok(list) => songs.extend(list.iter().map(SongOut::from)),
        Err(e) => notes.push(format!("kuwo: {e}")),
    }
    match kg {
        Ok(list) => songs.extend(list.iter().map(SongOut::from)),
        Err(e) => notes.push(format!("kugou: {e}")),
    }
    if songs.is_empty() {
        anyhow::bail!("没有结果（{}）", notes.join("; "));
    }
    eprintln!("[dl] “{query}” -> {} 首{}", songs.len(), if notes.is_empty() { String::new() } else { format!("（{}）", notes.join("; ")) });
    Ok(serde_json::json!({ "songs": songs }))
}

/// 一次完整下载：直链解析 → 下载（进度通知 + 可取消）→ 元数据门禁 → 落盘。
/// 元数据拿不到或写不进去 = 任务失败，产物清理（与 CLI 的门禁语义一致）。
async fn download_job(
    kuwo: &Option<Kuwo>,
    kugou: &Option<Kugou>,
    song: &SongRef,
    dest_dir: &std::path::Path,
    call: u64,
    out_tx: &mpsc::Sender<String>,
    cancel_rx: watch::Receiver<bool>,
) -> anyhow::Result<serde_json::Value> {
    let search_result = SearchResult::from(song);
    // 产物名在拿到直链前无从得知（tag 决定），但 fallback 名（title - artist）
    // 命中绝大多数场景：先快速查一下，省得 30MB 下完才发现库里已有
    let fallback_probe = format!("{} - {}", song.title, song.artist);
    for ext in ["flac", "mp3", "m4a", "ogg"] {
        let candidate = dest_dir.join(format!("{fallback_probe}.{ext}"));
        if candidate.exists() {
            anyhow::bail!("同名文件已存在：{}", candidate.display());
        }
    }
    match search_result.source {
        "kuwo" => {
            let source = kuwo.as_ref().ok_or_else(|| anyhow::anyhow!("kuwo 源不可用"))?;
            finish_download(source, &search_result, song, dest_dir, call, out_tx, cancel_rx).await
        }
        "kugou" => {
            let source = kugou.as_ref().ok_or_else(|| anyhow::anyhow!("kugou 源不可用"))?;
            finish_download(source, &search_result, song, dest_dir, call, out_tx, cancel_rx).await
        }
        other => anyhow::bail!("未知来源 “{other}”"),
    }
}

/// 下载后半程（源已定）：直链 → 下载 → 门禁 → 落盘。
async fn finish_download<S: MusicSource>(
    source: &S,
    search_result: &SearchResult,
    song: &SongRef,
    dest_dir: &std::path::Path,
    call: u64,
    out_tx: &mpsc::Sender<String>,
    cancel_rx: watch::Receiver<bool>,
) -> anyhow::Result<serde_json::Value> {
    let url = source.get_download_url(search_result).await?;
    eprintln!("[dl] 开始下载 “{}”", song.title);

    let notify = Arc::new(ProgressNotify {
        tx: out_tx.clone(),
        call,
        received: AtomicU64::new(0),
        last: Mutex::new(Instant::now() - Duration::from_millis(150)),
    });
    let notify_for_chunk = Arc::clone(&notify);
    let opts = DownloadOptions {
        dest_dir,
        on_chunk: &move |chunk, total| notify_for_chunk.push(chunk, total),
        cancelled: &|| *cancel_rx.borrow(),
    };
    let fallback_name = match &search_result.artist {
        Some(artist) => format!("{} - {}", search_result.title, artist),
        None => search_result.title.clone(),
    };
    let file_path = download_file(url, &fallback_name, &opts).await?;

    // 元数据是门禁：拿不到或写不进去，这次下载就算失败，清理产物
    send_notify(
        out_tx,
        call,
        "phase",
        serde_json::json!({"phase": "metadata"}),
    )
    .await;
    let meta_result = async {
        let meta = source.get_metadata(search_result).await?;
        write_metadata(&file_path, &meta).await
    }
    .await;
    if let Err(e) = meta_result {
        eprintln!("[dl] 元数据门禁失败（{e}），移除 {}", file_path.display());
        if let Err(rm_err) = tokio::fs::remove_file(&file_path).await {
            eprintln!("[dl] 清理失败：{rm_err}");
        }
        anyhow::bail!("元数据门禁失败：{e:#}");
    }

    Ok(serde_json::json!({"path": file_path}))
}

/// 回一行响应（成功 / 失败二选一）。
async fn send_reply(
    tx: &mpsc::Sender<String>,
    id: u64,
    reply: anyhow::Result<serde_json::Value>,
) {
    let line = match reply {
        Ok(result) => serde_json::json!({"id": id, "result": result}),
        Err(error) => serde_json::json!({"id": id, "error": format!("{error:#}")}),
    };
    let _ = tx.send(line.to_string()).await;
}

/// 回一行通知（progress / phase）。
async fn send_notify(
    tx: &mpsc::Sender<String>,
    call: u64,
    event: &str,
    data: serde_json::Value,
) {
    let line = serde_json::to_string(&Notify { event, call, data }).unwrap_or_default();
    let _ = tx.send(line).await;
}
