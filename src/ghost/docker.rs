use serde::Deserialize;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct DockerDiskInfo {
    pub is_available: bool,
    pub error_message: Option<String>,
    pub images_total_size: u64,
    pub images_reclaimable_size: u64,
    pub images_count: usize,
    pub containers_total_size: u64,
    pub containers_reclaimable_size: u64,
    pub containers_count: usize,
    pub volumes_total_size: u64,
    pub volumes_reclaimable_size: u64,
    pub volumes_count: usize,
    pub build_cache_total_size: u64,
    pub build_cache_reclaimable_size: u64,
    pub items: Vec<DockerItemSummary>,
}

#[derive(Debug, Clone)]
pub struct DockerItemSummary {
    pub category: &'static str,
    pub id_or_name: String,
    pub size: u64,
    pub is_reclaimable: bool,
    pub details: String,
}

// Internal raw deserialization structures from Docker daemon /system/df
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct DockerDfResponse {
    #[serde(default)]
    images: Vec<DockerImageItem>,
    #[serde(default)]
    containers: Vec<DockerContainerItem>,
    #[serde(default)]
    volumes: Vec<DockerVolumeItem>,
    #[serde(default)]
    build_cache: Vec<DockerBuildCacheItem>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
#[allow(dead_code)]
struct DockerImageItem {
    #[serde(default)]
    id: String,
    #[serde(default)]
    repo_tags: Option<Vec<String>>,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    shared_size: i64,
    #[serde(default)]
    containers: i64,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
#[allow(dead_code)]
struct DockerContainerItem {
    #[serde(default)]
    id: String,
    #[serde(default)]
    names: Option<Vec<String>>,
    #[serde(default)]
    size_rw: Option<i64>,
    #[serde(default)]
    size_root_fs: Option<i64>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct DockerVolumeItem {
    #[serde(default)]
    name: String,
    #[serde(default)]
    usage_data: Option<DockerVolumeUsageData>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct DockerVolumeUsageData {
    #[serde(default)]
    size: i64,
    #[serde(default)]
    ref_count: i64,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct DockerBuildCacheItem {
    #[serde(default, rename = "ID")]
    id: String,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    reclaimable: bool,
    #[serde(default)]
    description: Option<String>,
}

const DOCKER_SOCKET_PATH: &str = "/var/run/docker.sock";

fn send_docker_http_request(method: &str, endpoint: &str) -> Result<String, String> {
    if !Path::new(DOCKER_SOCKET_PATH).exists() {
        return Err("Docker socket /var/run/docker.sock does not exist".to_string());
    }

    let mut stream = UnixStream::connect(DOCKER_SOCKET_PATH)
        .map_err(|e| format!("Cannot connect to docker socket: {}", e))?;

    stream
        .set_read_timeout(Some(Duration::from_secs(4)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(4)))
        .map_err(|e| e.to_string())?;

    let request = format!(
        "{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        method, endpoint
    );

    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("Failed to write to docker socket: {}", e))?;

    let mut response_bytes = Vec::new();
    stream
        .read_to_end(&mut response_bytes)
        .map_err(|e| format!("Failed to read from docker socket: {}", e))?;

    parse_docker_http_response(&response_bytes)
}

fn parse_docker_http_response(response_bytes: &[u8]) -> Result<String, String> {
    let sep = response_bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "Invalid HTTP response from docker socket".to_string())?;

    let header_str = String::from_utf8_lossy(&response_bytes[..sep]);
    let raw_body = &response_bytes[sep + 4..];

    let status_line = header_str.lines().next().unwrap_or("");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);

    if !(200..300).contains(&status_code) {
        return Err(format!(
            "Docker API error (HTTP {}): {}",
            status_code,
            String::from_utf8_lossy(raw_body).trim()
        ));
    }

    let is_chunked = header_str.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });

    let body = if is_chunked {
        let mut dechunked: Vec<u8> = Vec::new();
        let mut remaining = raw_body;
        loop {
            let line_end = remaining
                .windows(2)
                .position(|w| w == b"\r\n")
                .ok_or_else(|| "Missing Docker chunk header".to_string())?;
            let hex_str = String::from_utf8_lossy(&remaining[..line_end]);
            let hex_clean = hex_str.split(';').next().unwrap_or("").trim();
            let chunk_len = usize::from_str_radix(hex_clean, 16)
                .map_err(|_| "Invalid Docker chunk size".to_string())?;
            if chunk_len == 0 {
                break;
            }
            let data_start = line_end + 2;
            let data_end = data_start
                .checked_add(chunk_len)
                .ok_or_else(|| "Docker chunk size overflow".to_string())?;
            let chunk_end = data_end
                .checked_add(2)
                .ok_or_else(|| "Docker chunk size overflow".to_string())?;
            if remaining.get(data_end..chunk_end) != Some(b"\r\n") {
                return Err("Truncated Docker chunk or missing CRLF".to_string());
            }
            dechunked.extend_from_slice(&remaining[data_start..data_end]);
            remaining = &remaining[chunk_end..];
        }
        String::from_utf8_lossy(&dechunked).into_owned()
    } else {
        String::from_utf8_lossy(raw_body).into_owned()
    };

    Ok(body)
}

pub fn parse_docker_df_json(json_body: &str) -> Result<DockerDiskInfo, String> {
    let mut info = DockerDiskInfo::default();
    let parsed: DockerDfResponse = serde_json::from_str(json_body)
        .map_err(|e| format!("Failed to parse Docker response: {}", e))?;

    info.is_available = true;
    info.images_count = parsed.images.len();
    for img in parsed.images {
        let size = img.size.max(0) as u64;
        info.images_total_size += size;
        let is_untagged = img.repo_tags.as_ref().is_none_or(|tags| {
            tags.is_empty() || tags.iter().all(|t| t == "<none>:<none>" || t == "<none>")
        });
        let is_dangling = is_untagged && img.containers == 0;
        if is_dangling {
            info.images_reclaimable_size += size;
        }
        let tag = img
            .repo_tags
            .as_ref()
            .and_then(|t| t.first().cloned())
            .unwrap_or_else(|| "<none>".to_string());
        info.items.push(DockerItemSummary {
            category: "Image",
            id_or_name: if tag != "<none>" {
                tag
            } else {
                img.id.chars().take(12).collect()
            },
            size,
            is_reclaimable: is_dangling,
            details: if is_dangling {
                "Dangling / Unused".to_string()
            } else {
                format!("Used by {} containers", img.containers)
            },
        });
    }

    info.containers_count = parsed.containers.len();
    for c in parsed.containers {
        let rw_size = c.size_rw.unwrap_or(0).max(0) as u64;
        info.containers_total_size += rw_size;
        let is_stopped = matches!(
            c.state.as_deref(),
            Some("exited") | Some("dead") | Some("created")
        ) || c.status.as_deref().is_some_and(|s| s.starts_with("Exited"));
        if is_stopped {
            info.containers_reclaimable_size += rw_size;
        }
        let name = c
            .names
            .as_ref()
            .and_then(|n| n.first().cloned())
            .unwrap_or_else(|| c.id.chars().take(12).collect());
        info.items.push(DockerItemSummary {
            category: "Container",
            id_or_name: name.trim_start_matches('/').to_string(),
            size: rw_size,
            is_reclaimable: is_stopped,
            details: format!(
                "Status: {}",
                c.status.unwrap_or_else(|| "Unknown".to_string())
            ),
        });
    }

    info.volumes_count = parsed.volumes.len();
    for v in parsed.volumes {
        let (vol_size, is_reclaimable) = match v.usage_data {
            Some(ud) => (ud.size.max(0) as u64, ud.ref_count <= 0),
            None => (0, true),
        };
        info.volumes_total_size += vol_size;
        if is_reclaimable {
            info.volumes_reclaimable_size += vol_size;
        }
        info.items.push(DockerItemSummary {
            category: "Volume",
            id_or_name: v.name.chars().take(24).collect(),
            size: vol_size,
            is_reclaimable,
            details: if is_reclaimable {
                "Dangling / Unattached".to_string()
            } else {
                "Active".to_string()
            },
        });
    }

    for bc in parsed.build_cache {
        let size = bc.size.max(0) as u64;
        info.build_cache_total_size += size;
        if bc.reclaimable {
            info.build_cache_reclaimable_size += size;
        }
        info.items.push(DockerItemSummary {
            category: "BuildCache",
            id_or_name: bc.id.chars().take(12).collect(),
            size,
            is_reclaimable: bc.reclaimable,
            details: bc
                .description
                .unwrap_or_else(|| "Build cache entry".to_string()),
        });
    }

    Ok(info)
}

pub fn fetch_docker_disk_info() -> DockerDiskInfo {
    match send_docker_http_request("GET", "/system/df") {
        Ok(body) => parse_docker_df_json(&body).unwrap_or_else(|err| DockerDiskInfo {
            is_available: false,
            error_message: Some(err),
            ..Default::default()
        }),
        Err(e) => DockerDiskInfo {
            is_available: false,
            error_message: Some(e),
            ..Default::default()
        },
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct DockerPruneResponse {
    #[serde(default)]
    space_reclaimed: u64,
}

pub fn prune_docker_dangling() -> Result<String, String> {
    // Reporting and mutation must use the same socket, regardless of CLI context/DOCKER_HOST.
    prune_with_request(send_docker_http_request)
}

fn prune_with_request(
    mut request: impl FnMut(&str, &str) -> Result<String, String>,
) -> Result<String, String> {
    let mut completed = Vec::new();
    let mut failed = Vec::new();
    let mut total_reclaimed = 0u64;
    for (category, endpoint) in [
        (
            "images",
            "/images/prune?filters=%7B%22dangling%22%3A%7B%22true%22%3Atrue%7D%7D",
        ),
        ("containers", "/containers/prune"),
        ("volumes", "/volumes/prune"),
        ("build cache", "/build/prune"),
    ] {
        match request("POST", endpoint).and_then(|body| {
            serde_json::from_str::<DockerPruneResponse>(&body)
                .map_err(|e| format!("Invalid prune response: {}", e))
        }) {
            Ok(result) => {
                total_reclaimed = total_reclaimed.saturating_add(result.space_reclaimed);
                completed.push(category);
            }
            Err(error) => failed.push(format!("{}: {}", category, error)),
        }
    }
    let summary = format!(
        "Pruned {} (freed {})",
        if completed.is_empty() {
            "none".to_string()
        } else {
            completed.join(", ")
        },
        crate::fs::entry::format_size(total_reclaimed)
    );
    if failed.is_empty() {
        Ok(summary)
    } else {
        Err(format!("{}; failed: {}", summary, failed.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_response_preserves_split_utf8() {
        let mut response =
            b"HTTP/1.1 200 OK\r\ntRaNsFeR-EnCoDiNg: Chunked\r\n\r\n1;ext=yes\r\n".to_vec();
        response.push(0xc3);
        response.extend_from_slice(b"\r\n1\r\n\xa9\r\n0\r\n\r\n");
        assert_eq!(parse_docker_http_response(&response).unwrap(), "é");
    }

    #[test]
    fn malformed_chunks_and_http_errors_are_rejected() {
        for body in [
            "1\r\na",
            "4\r\na\r\n",
            "1\r\naXX",
            "xyz\r\n",
            "1\r\na\r\n",
            "ffffffffffffffff\r\n",
        ] {
            let response = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{body}");
            assert!(
                parse_docker_http_response(response.as_bytes()).is_err(),
                "{body:?}"
            );
        }
        let response = b"HTTP/1.1 403 Forbidden\r\n\r\naccess denied";
        let error = parse_docker_http_response(response).unwrap_err();
        assert!(error.contains("403"));
        assert!(error.contains("access denied"));
    }

    #[test]
    fn prune_failure_does_not_switch_execution_paths() {
        let mut calls = Vec::new();
        let error = prune_with_request(|method, endpoint| {
            assert_eq!(method, "POST");
            calls.push(endpoint.to_string());
            Err("Docker API error (HTTP 403): access denied".to_string())
        })
        .unwrap_err();
        assert_eq!(calls.len(), 4);
        assert!(calls[0].starts_with("/images/prune?filters="));
        assert_eq!(
            &calls[1..],
            ["/containers/prune", "/volumes/prune", "/build/prune"]
        );
        assert!(error.contains("Pruned none"));
        assert!(error.contains("403"));
    }

    #[test]
    fn prune_reports_success_partial_failure_and_invalid_responses() {
        let success =
            prune_with_request(|_, _| Ok(r#"{"SpaceReclaimed":10}"#.to_string())).unwrap();
        assert!(success.contains("images, containers, volumes, build cache"));
        assert!(success.contains("40 B"));
        let partial = prune_with_request(|_, endpoint| {
            if endpoint == "/containers/prune" {
                Err("connection closed".to_string())
            } else {
                Ok(r#"{"SpaceReclaimed":10}"#.to_string())
            }
        })
        .unwrap_err();
        assert!(partial.contains("Pruned images, volumes, build cache (freed 30 B)"));
        assert!(partial.contains("failed: containers: connection closed"));
        let invalid = prune_with_request(|_, _| Ok("invalid JSON".to_string())).unwrap_err();
        assert!(invalid.contains("Pruned none"));
        assert!(invalid.contains("Invalid prune response"));
    }
}
