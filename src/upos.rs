use crate::video::read_chunk;
use crate::video::Video;
use anyhow::{bail, Result};
use async_std::fs::File;
use futures_util::{StreamExt, TryStreamExt};
use reqwest::header;
use reqwest_middleware::ClientBuilder;
use reqwest_retry::policies::ExponentialBackoff;
use reqwest_retry::RetryTransientMiddleware;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Upos<'a> {
    upload_id: &'a str,
    chunks: usize,
    total: u64,
    chunk: usize,
    size: usize,
    part_number: usize,
    start: usize,
    end: usize,
}

impl Upos<'_> {
    pub async fn upload(
        file: File,
        path: &async_std::path::Path,
        ret: serde_json::Value,
        mut callback: impl FnMut(Instant, u64, usize) -> bool,
    ) -> Result<Video> {
        let chunk_size = ret["chunk_size"].as_u64().unwrap() as usize;
        let auth = ret["auth"].as_str().unwrap();
        let endpoint = ret["endpoint"].as_str().unwrap();
        let biz_id = &ret["biz_id"];
        let upos_uri = ret["upos_uri"].as_str().unwrap();
        let url = format!("https:{}/{}", endpoint, upos_uri.replace("upos://", "")); // 视频上传路径
        println!("{}", url);
        let mut headers = header::HeaderMap::new();
        headers.insert("X-Upos-Auth", header::HeaderValue::from_str(auth)?);
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/63.0.3239.108")
            .default_headers(headers)
            .timeout(Duration::new(60, 0))
            .build()
            .unwrap();
        let retry_policy = ExponentialBackoff::builder().build_with_max_retries(3);
        let client = &ClientBuilder::new(client)
            // Retry failed requests.
            .with(RetryTransientMiddleware::new_with_policy(retry_policy))
            .build();
        let upload_id: serde_json::Value = client
            .post(format!("{}?uploads&output=json", url))
            .send()
            .await?
            .json()
            .await?;
        let upload_id = &upload_id["upload_id"];

        let total_size = file.metadata().await?.len();
        // let parts = Vec::new();
        // let parts_cell = &RefCell::new(parts);
        let chunks_num = (total_size as f64 / chunk_size as f64).ceil() as usize; // 获取分块数量
        let url = &url;
        // let file = tokio::io::BufReader::with_capacity(chunk_size, file);

        let instant = Instant::now();
        let mut parts = Vec::with_capacity(chunks_num);
        let stream = read_chunk(file, chunk_size)
            // let mut chunks = read_chunk(file, chunk_size)
            .enumerate()
            .map(|(i, chunk)| {
                let chunk = chunk.unwrap();
                let len = chunk.len();
                println!("{}", len);
                let params = Upos {
                    upload_id: upload_id.as_str().unwrap(),
                    chunks: chunks_num,
                    total: total_size,
                    chunk: i,
                    size: len,
                    part_number: i + 1,
                    start: i * chunk_size,
                    end: i * chunk_size + len,
                };
                async move {
                    client.put(url).query(&params).body(chunk).send().await?;
                    Ok::<_, reqwest_middleware::Error>((
                        json!({"partNumber": params.chunk + 1, "eTag": "etag"}),
                        len,
                    ))
                }
            })
            .buffer_unordered(3);
        // .for_each_concurrent()
        // .try_collect().await?;
        // let mut parts = Vec::with_capacity(chunks_num);
        tokio::pin!(stream);
        while let Some((part, size)) = stream.try_next().await? {
            parts.push(part);
            // (callback)(instant, total_size, size);
            if callback(instant, total_size, size) {
                bail!("移除视频");
            }
        }

        println!(
            "{:.2} MB/s.",
            total_size as f64 / 1000. / instant.elapsed().as_millis() as f64
        );
        // println!("{:?}", parts_cell.borrow());
        let value = json!({
            "name": path.file_name().and_then(OsStr::to_str),
            "uploadId": upload_id,
            "biz_id": biz_id,
            "output": "json",
            "profile": "ugcupos/bup"
        });
        // let res: serde_json::Value = self.client.post(url).query(&value).json(&json!({"parts": *parts_cell.borrow()}))
        let res: serde_json::Value = client
            .post(url)
            .query(&value)
            .json(&json!({ "parts": parts }))
            .send()
            .await?
            .json()
            .await?;
        if res["OK"] != 1 {
            bail!("{}", res)
        }
        Ok(Video {
            title: path
                .file_stem()
                .and_then(OsStr::to_str)
                .map(|s| s.to_string()),
            filename: Path::new(upos_uri)
                .file_stem()
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            desc: "",
        })
    }
}
