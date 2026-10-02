use serde::Deserialize;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
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

    let response = String::from_utf8_lossy(&response_bytes);
    let (header, raw_body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "Invalid HTTP response from docker socket".to_string())?;

    let status_line = header.lines().next().unwrap_or("");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);

    if !(200..300).contains(&status_code) {
        return Err(format!(
            "Docker API error (HTTP {}): {}",
            status_code,
            raw_body.trim()
        ));
    }

    let body = if header.contains("Transfer-Encoding: chunked") {
        let mut dechunked = String::new();
        let mut remaining = raw_body;
        while let Some(line_end) = remaining.find("\r\n") {
            let hex_len_str = remaining[..line_end].trim();
            if let Ok(chunk_len) = usize::from_str_radix(hex_len_str, 16) {
                if chunk_len == 0 {
                    break;
                }
                let data_start = line_end + 2;
                let data_end = data_start + chunk_len;
                if data_end <= remaining.len() {
                    dechunked.push_str(&remaining[data_start..data_end]);
                    remaining = &remaining[data_end + 2..];
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        dechunked
    } else {
        raw_body.to_string()
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
    let mut messages = Vec::new();
    let mut total_reclaimed = 0u64;

    // 1. Prune dangling images
    if let Ok(res) = send_docker_http_request(
        "POST",
        "/images/prune?filters=%7B%22dangling%22%3A%7B%22true%22%3Atrue%7D%7D",
    ) {
        if let Ok(p) = serde_json::from_str::<DockerPruneResponse>(&res) {
            total_reclaimed += p.space_reclaimed;
        }
        messages.push("images");
    }
    // 2. Prune stopped containers
    if let Ok(res) = send_docker_http_request("POST", "/containers/prune") {
        if let Ok(p) = serde_json::from_str::<DockerPruneResponse>(&res) {
            total_reclaimed += p.space_reclaimed;
        }
        messages.push("containers");
    }
    // 3. Prune dangling volumes
    if let Ok(res) = send_docker_http_request("POST", "/volumes/prune") {
        if let Ok(p) = serde_json::from_str::<DockerPruneResponse>(&res) {
            total_reclaimed += p.space_reclaimed;
        }
        messages.push("volumes");
    }
    // 4. Prune build cache
    if let Ok(res) = send_docker_http_request("POST", "/build/prune") {
        if let Ok(p) = serde_json::from_str::<DockerPruneResponse>(&res) {
            total_reclaimed += p.space_reclaimed;
        }
        messages.push("build cache");
    }

    if messages.is_empty() {
        // Fallback to docker CLI
        let output = Command::new("docker")
            .args(["system", "prune", "-f", "--volumes"])
            .output()
            .map_err(|e| format!("Failed to run docker CLI prune: {}", e))?;
        if output.status.success() {
            Ok("Successfully ran docker system prune".to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    } else {
        use crate::fs::entry::format_size;
        Ok(format!(
            "Pruned {} (freed {})",
            messages.join(", "),
            format_size(total_reclaimed)
        ))
    }
}
