//! 真实网络的集成测试：搜索 → 解析下载直链 → Range 探测音频魔数 → 元数据。
//! 依赖外部服务（酷我/酷狗/baka），网络环境变化可能导致失败。

use rmus_dl::sources::{MusicSource, kugou::Kugou, kuwo::Kuwo};

const KEYWORD: &str = "晴天 周杰伦";

/// 用 Range 头取前几个字节，验证直链真实可下且是音频（flac 或 mp3）。
async fn probe_audio_magic(url: &reqwest::Url) -> anyhow::Result<()> {
    let client = reqwest::Client::builder().build()?;
    let resp = client
        .get(url.clone())
        .header("Range", "bytes=0-15")
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await?
        .error_for_status()?;
    let bytes = resp.bytes().await?;
    assert!(
        bytes.len() >= 4,
        "expected at least 4 bytes from media url, got {}",
        bytes.len()
    );
    let is_flac = &bytes[..4] == b"fLaC";
    let is_mp3 = &bytes[..3] == b"ID3" || (bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0);
    assert!(
        is_flac || is_mp3,
        "media url is neither FLAC nor MP3: {:?}",
        &bytes[..4]
    );
    Ok(())
}

#[tokio::test]
async fn kuwo_full_chain_search_download_metadata() {
    let kuwo = Kuwo::new(false).unwrap();
    let results = kuwo.search(KEYWORD).await.expect("kuwo search should work");
    assert!(!results.is_empty(), "kuwo search returned no results");
    assert!(
        results.iter().all(|r| r.source == "kuwo" && !r.song_id.is_empty()),
        "every result should carry a rid"
    );

    let first = &results[0];
    let url = kuwo
        .get_download_url(first)
        .await
        .expect("kuwo should resolve a download url");
    probe_audio_magic(&url)
        .await
        .expect("kuwo download url should serve a real audio stream");

    // 门禁语义：元数据拿不到就算失败（内部已重试 3 次）
    let meta = kuwo
        .get_metadata(first)
        .await
        .expect("kuwo metadata should be fetchable (gate requirement)");
    meta.gate().expect("metadata should pass the gate");
    if let Some(lyric) = meta.lyric {
        assert!(
            lyric.contains("[00:"),
            "lyric should be LRC-formatted: {lyric:.80}"
        );
    }
}

#[tokio::test]
async fn kugou_search_and_baka_download() {
    let kugou = Kugou::new(false).unwrap();
    let results = kugou
        .search(KEYWORD)
        .await
        .expect("kugou search should work");
    assert!(!results.is_empty(), "kugou search returned no results");

    let first = &results[0];
    let url = kugou
        .get_download_url(first)
        .await
        .expect("kugou resolvers should give a download url");
    probe_audio_magic(&url)
        .await
        .expect("resolved url should serve a real audio stream");

    // 门禁语义：歌词或封面至少一样（kugou 封面来自搜索结果 union_cover）
    let meta = kugou
        .get_metadata(first)
        .await
        .expect("kugou metadata should be fetchable (gate requirement)");
    meta.gate().expect("metadata should pass the gate");
}
