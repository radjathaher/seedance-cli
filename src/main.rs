use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose};
use clap::{Args, Parser, Subcommand, ValueEnum};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    env, fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

const DEFAULT_BASE_URL: &str = "https://api.segmind.com";
const DEFAULT_STORAGE_URL: &str = "https://workflows-api.segmind.com";

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "seedance",
    version,
    about = "Seedance 2.0 CLI for Segmind video generation",
    long_about = "Seedance 2.0 CLI for Segmind video generation.\n\nDefault flow: upload local references, submit an async Seedance job, poll until completion, and download the generated video to --out.",
    after_help = "Examples:\n  seedance generate --prompt \"Neon city at night\" --speed fast --resolution 480p --duration-seconds 4 --out video.mp4\n  seedance generate --prompt \"Animate this\" --image ./ref.png --out video.mp4\n  seedance generate --prompt \"Batch job\" --no-wait --pretty\n  seedance task wait --task-id req_123 --out video.mp4"
)]
struct Cli {
    #[arg(long, global = true, default_value = DEFAULT_BASE_URL, help = "Segmind API base URL")]
    base_url: String,
    #[arg(long, global = true, default_value = DEFAULT_STORAGE_URL, help = "Segmind storage API base URL")]
    storage_url: String,
    #[arg(long, global = true, help = "Pretty-print JSON output")]
    pretty: bool,
    #[arg(long, global = true, help = "Print raw vendor JSON when available")]
    raw: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(about = "Generate a Seedance 2.0 video")]
    Generate(GenerateArgs),
    #[command(about = "Upload local assets to Segmind storage")]
    Upload(UploadArgs),
    #[command(about = "Inspect or wait on async Segmind tasks")]
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    #[command(about = "Print bundled Seedance price notes")]
    Pricing,
}

#[derive(Debug, Clone, Args)]
struct GenerateArgs {
    #[arg(long, help = "Text prompt")]
    prompt: String,
    #[arg(long, value_enum, default_value_t = Speed::Fast, help = "Seedance model speed")]
    speed: Speed,
    #[arg(long, value_enum, default_value_t = Resolution::R480p, help = "Output resolution")]
    resolution: Resolution,
    #[arg(
        long,
        default_value_t = 5,
        help = "Requested video duration in seconds"
    )]
    duration_seconds: u32,
    #[arg(
        long,
        default_value = "16:9",
        help = "Output aspect ratio, for example 16:9, 9:16, 1:1"
    )]
    aspect_ratio: String,
    #[arg(
        long = "image",
        help = "Reference image URL or local path; repeat up to 9"
    )]
    images: Vec<String>,
    #[arg(
        long = "video",
        help = "Reference video URL or local path; repeat up to 3"
    )]
    videos: Vec<String>,
    #[arg(
        long = "audio",
        help = "Reference audio URL or local path; repeat up to 3"
    )]
    audios: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Ask Seedance to generate audio when supported"
    )]
    generate_audio: bool,
    #[arg(long, default_value_t = -1, help = "Seed; -1 lets provider choose")]
    seed: i64,
    #[arg(
        long,
        default_value_t = false,
        help = "Request last frame output when supported"
    )]
    return_last_frame: bool,
    #[arg(
        long,
        default_value = "false",
        help = "Provider moderation flag passed through to Segmind"
    )]
    skip_moderation: String,
    #[arg(long, help = "Write generated video to this path")]
    out: Option<PathBuf>,
    #[arg(long, default_value_t = 2, help = "Polling interval in seconds")]
    poll_interval_secs: u64,
    #[arg(long, default_value_t = 900, help = "Maximum wait time in seconds")]
    max_wait_secs: u64,
    #[arg(long, help = "Submit only and print request metadata without polling")]
    no_wait: bool,
}

#[derive(Debug, Clone, Args)]
struct UploadArgs {
    #[arg(
        long = "image",
        help = "Image URL or local path; URLs pass through unchanged"
    )]
    images: Vec<String>,
    #[arg(
        long = "video",
        help = "Video URL or local path; URLs pass through unchanged"
    )]
    videos: Vec<String>,
    #[arg(
        long = "audio",
        help = "Audio URL or local path; URLs pass through unchanged"
    )]
    audios: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    #[command(about = "Fetch current task status once")]
    Get(TaskGetArgs),
    #[command(about = "Poll a task until completion and optionally download output")]
    Wait(TaskWaitArgs),
}

#[derive(Debug, Clone, Args)]
struct TaskGetArgs {
    #[arg(long)]
    task_id: String,
}

#[derive(Debug, Clone, Args)]
struct TaskWaitArgs {
    #[arg(long)]
    task_id: String,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long, default_value_t = 2)]
    poll_interval_secs: u64,
    #[arg(long, default_value_t = 900)]
    max_wait_secs: u64,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum Speed {
    Fast,
    Standard,
}

impl Speed {
    fn slug(self) -> &'static str {
        match self {
            Speed::Fast => "seedance-2.0-fast",
            Speed::Standard => "seedance-2.0",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq, Serialize)]
enum Resolution {
    #[value(name = "480p")]
    #[serde(rename = "480p")]
    R480p,
    #[value(name = "720p")]
    #[serde(rename = "720p")]
    R720p,
    #[value(name = "1080p")]
    #[serde(rename = "1080p")]
    R1080p,
}

impl Resolution {
    fn as_str(self) -> &'static str {
        match self {
            Resolution::R480p => "480p",
            Resolution::R720p => "720p",
            Resolution::R1080p => "1080p",
        }
    }
}

struct ApiClient {
    client: Client,
    api_key: String,
    base_url: String,
    storage_url: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct SubmitResponse {
    request_id: String,
    status_url: String,
    response_url: String,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

impl ApiClient {
    fn new(base_url: String, storage_url: String) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .user_agent(concat!("seedance-cli/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("build http client")?,
            api_key: read_secret("SEGMIND_API_KEY")?,
            base_url,
            storage_url,
        })
    }

    fn post_json(&self, url: String, body: Value) -> Result<Value> {
        let response = self
            .client
            .post(url)
            .header("accept", "application/json")
            .header("content-type", "application/json")
            .header("x-api-key", &self.api_key)
            .json(&body)
            .send()
            .context("send request")?;
        decode_response(response)
    }

    fn get_json_url(&self, url: &str) -> Result<Value> {
        let response = self
            .client
            .get(url)
            .header("accept", "application/json")
            .header("x-api-key", &self.api_key)
            .send()
            .context("send request")?;
        decode_response_terminal_tolerant(response)
    }

    fn submit(&self, slug: &str, body: Value) -> Result<SubmitResponse> {
        let url = format!("{}/v2/{}", self.base_url.trim_end_matches('/'), slug);
        let body = self.post_json(url, body)?;
        serde_json::from_value(body).context("decode submit response")
    }

    fn status_url(&self, task_id: &str) -> String {
        format!(
            "{}/v2/requests/{}/status",
            self.base_url.trim_end_matches('/'),
            task_id
        )
    }

    fn response_url(&self, task_id: &str) -> String {
        format!(
            "{}/v2/requests/{}",
            self.base_url.trim_end_matches('/'),
            task_id
        )
    }

    fn upload_one(&self, path: &str) -> Result<String> {
        if is_remote_ref(path) {
            return Ok(path.to_string());
        }
        let data_url = file_to_data_url(path)?;
        let url = format!("{}/upload-asset", self.storage_url.trim_end_matches('/'));
        let response = self.post_json(url, json!({ "data_urls": [data_url] }))?;
        first_uploaded_url(&response)
            .with_context(|| format!("upload response missing file URL for {path}"))
    }

    fn download_to(&self, url: &str, out: &Path) -> Result<()> {
        let response = self.client.get(url).send().context("download output")?;
        let status = response.status();
        let bytes = response.bytes().context("read output body")?;
        if !status.is_success() {
            bail!("http {status} while downloading {url}");
        }
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        fs::write(out, bytes).with_context(|| format!("write {}", out.display()))
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Generate(args) => {
            let api = ApiClient::new(cli.base_url, cli.storage_url)?;
            handle_generate(&api, args, cli.pretty, cli.raw)
        }
        Commands::Upload(args) => {
            let api = ApiClient::new(cli.base_url, cli.storage_url)?;
            let output = json!({
                "reference_images": upload_refs(&api, &args.images, 9, "images")?,
                "reference_videos": upload_refs(&api, &args.videos, 3, "videos")?,
                "reference_audios": upload_refs(&api, &args.audios, 3, "audios")?,
            });
            print_json(&output, cli.pretty)
        }
        Commands::Task {
            command: TaskCommand::Get(args),
        } => {
            let api = ApiClient::new(cli.base_url, cli.storage_url)?;
            let status = api.get_json_url(&api.status_url(&args.task_id))?;
            print_json(&status, cli.pretty)
        }
        Commands::Task {
            command: TaskCommand::Wait(args),
        } => {
            let api = ApiClient::new(cli.base_url, cli.storage_url)?;
            let result = wait_for_result(
                &api,
                &args.task_id,
                args.max_wait_secs,
                args.poll_interval_secs,
            )?;
            if let Some(out) = args.out {
                let url = extract_output_url(&result)
                    .context("completed result did not contain a downloadable output URL")?;
                api.download_to(&url, &out)?;
                if cli.raw {
                    print_json(&result, cli.pretty)?;
                } else {
                    print_json(
                        &json!({ "task_id": args.task_id, "output_url": url, "out": out }),
                        cli.pretty,
                    )?;
                }
            } else {
                print_json(&result, cli.pretty)?;
            }
            Ok(())
        }
        Commands::Pricing => print_pricing(),
    }
}

fn handle_generate(api: &ApiClient, args: GenerateArgs, pretty: bool, raw: bool) -> Result<()> {
    validate_generate(&args)?;
    let reference_images = upload_refs(api, &args.images, 9, "images")?;
    let reference_videos = upload_refs(api, &args.videos, 3, "videos")?;
    let reference_audios = upload_refs(api, &args.audios, 3, "audios")?;
    let body = json!({
        "prompt": args.prompt,
        "reference_images": reference_images,
        "reference_videos": reference_videos,
        "reference_audios": reference_audios,
        "duration": args.duration_seconds,
        "resolution": args.resolution.as_str(),
        "aspect_ratio": args.aspect_ratio,
        "generate_audio": args.generate_audio,
        "seed": args.seed,
        "return_last_frame": args.return_last_frame,
        "skip_moderation": args.skip_moderation,
    });
    let submit = api.submit(args.speed.slug(), body)?;
    if args.no_wait {
        return print_json(&serde_json::to_value(submit)?, pretty);
    }
    let out = args
        .out
        .context("--out is required unless --no-wait is set")?;
    let result = wait_for_result(
        api,
        &submit.request_id,
        args.max_wait_secs,
        args.poll_interval_secs,
    )?;
    let url = extract_output_url(&result)
        .context("completed result did not contain a downloadable output URL")?;
    api.download_to(&url, &out)?;
    if raw {
        print_json(&result, pretty)
    } else {
        print_json(
            &json!({
                "task_id": submit.request_id,
                "status": "COMPLETED",
                "output_url": url,
                "out": out,
            }),
            pretty,
        )
    }
}

fn validate_generate(args: &GenerateArgs) -> Result<()> {
    if args.speed == Speed::Fast && args.resolution == Resolution::R1080p {
        bail!("Seedance 2.0 Fast supports 480p or 720p, not 1080p");
    }
    validate_count(&args.images, 9, "images")?;
    validate_count(&args.videos, 3, "videos")?;
    validate_count(&args.audios, 3, "audios")?;
    if !args.no_wait && args.out.is_none() {
        bail!("--out is required unless --no-wait is set");
    }
    Ok(())
}

fn validate_count(values: &[String], max: usize, name: &str) -> Result<()> {
    if values.len() > max {
        bail!("too many {name}: got {}, max {max}", values.len());
    }
    Ok(())
}

fn upload_refs(api: &ApiClient, values: &[String], max: usize, name: &str) -> Result<Vec<String>> {
    validate_count(values, max, name)?;
    values.iter().map(|value| api.upload_one(value)).collect()
}

fn wait_for_result(
    api: &ApiClient,
    task_id: &str,
    max_wait_secs: u64,
    poll_interval_secs: u64,
) -> Result<Value> {
    let start = Instant::now();
    let interval = Duration::from_secs(poll_interval_secs.max(1));
    let timeout = Duration::from_secs(max_wait_secs);
    loop {
        let status = api.get_json_url(&api.status_url(task_id))?;
        match status.get("status").and_then(Value::as_str) {
            Some("COMPLETED") => return api.get_json_url(&api.response_url(task_id)),
            Some("FAILED") => bail!("task failed: {}", status.get("error").unwrap_or(&status)),
            Some(_) | None => {
                if start.elapsed() >= timeout {
                    bail!("task {task_id} did not complete within {max_wait_secs}s");
                }
                thread::sleep(interval);
            }
        }
    }
}

fn read_secret(name: &str) -> Result<String> {
    if let Ok(value) = env::var(name) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let path = format!("/run/secrets/{name}");
    if let Ok(value) = fs::read_to_string(&path) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    bail!("{name} missing; set ${name} or create /run/secrets/{name}")
}

fn is_remote_ref(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://") || value.starts_with("data:")
}

fn file_to_data_url(path: &str) -> Result<String> {
    let path = Path::new(path);
    let mime = mime_for_path(path)?;
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(format!(
        "data:{mime};base64,{}",
        general_purpose::STANDARD.encode(bytes)
    ))
}

fn mime_for_path(path: &Path) -> Result<&'static str> {
    if !path.exists() {
        bail!("file not found: {}", path.display());
    }
    if !path.is_file() {
        bail!("not a file: {}", path.display());
    }
    let ext = path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Ok("image/png"),
        "jpg" | "jpeg" | "jfif" | "pjp" => Ok("image/jpeg"),
        "webp" => Ok("image/webp"),
        "gif" => Ok("image/gif"),
        "bmp" => Ok("image/bmp"),
        "svg" | "svgz" => Ok("image/svg+xml"),
        "heic" => Ok("image/heic"),
        "heif" => Ok("image/heif"),
        "tif" | "tiff" => Ok("image/tiff"),
        "mp3" => Ok("audio/mpeg"),
        "aiff" => Ok("audio/aiff"),
        "wma" => Ok("audio/x-ms-wma"),
        "au" => Ok("audio/basic"),
        "mp4" => Ok("video/mp4"),
        "mov" => Ok("video/quicktime"),
        "avi" => Ok("video/x-msvideo"),
        "mkv" => Ok("video/x-matroska"),
        "wmv" => Ok("video/x-ms-wmv"),
        "flv" => Ok("video/x-flv"),
        "webm" => Ok("video/webm"),
        "mpeg" | "mpg" => Ok("video/mpeg"),
        _ => bail!("unsupported media extension for {}", path.display()),
    }
}

fn first_uploaded_url(value: &Value) -> Option<String> {
    value
        .get("file_urls")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(Value::as_str)
        .or_else(|| value.get("url").and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn extract_output_url(result: &Value) -> Option<String> {
    if let Some(output) = result.get("output") {
        return find_media_url(output);
    }
    find_media_url(result)
}

fn find_media_url(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if is_downloadable_url(s) => Some(s.clone()),
        Value::Array(values) => values.iter().find_map(find_media_url),
        Value::Object(map) => {
            for key in ["video", "video_url", "url", "output", "result", "file_url"] {
                if let Some(found) = map.get(key).and_then(find_media_url) {
                    return Some(found);
                }
            }
            map.values().find_map(find_media_url)
        }
        _ => None,
    }
}

fn is_downloadable_url(s: &str) -> bool {
    (s.starts_with("http://") || s.starts_with("https://"))
        && [".mp4", ".mov", ".webm", ".mkv", ".avi"]
            .iter()
            .any(|ext| s.to_ascii_lowercase().contains(ext))
}

fn decode_response(response: reqwest::blocking::Response) -> Result<Value> {
    let status = response.status();
    let body = response.text().context("read response body")?;
    let parsed = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({ "raw": body }));
    if !status.is_success() {
        bail!("http {status}: {parsed}");
    }
    Ok(parsed)
}

fn decode_response_terminal_tolerant(response: reqwest::blocking::Response) -> Result<Value> {
    let status = response.status();
    let body = response.text().context("read response body")?;
    let parsed = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({ "raw": body }));
    if parsed
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|s| s == "COMPLETED" || s == "FAILED")
    {
        return Ok(parsed);
    }
    if !status.is_success() {
        bail!("http {status}: {parsed}");
    }
    Ok(parsed)
}

fn print_json(value: &Value, pretty: bool) -> Result<()> {
    if pretty {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", serde_json::to_string(value)?);
    }
    Ok(())
}

fn print_pricing() -> Result<()> {
    println!(
        "Seedance 2.0 Segmind reference pricing notes, USD/sec; video-input is billed on input+output duration."
    );
    println!("speed,resolution,no_video,with_video");
    println!("fast,480p,0.0538-0.0562,0.0317-0.0331");
    println!("fast,720p,0.1210-0.1217,0.0713-0.0717");
    println!("standard,480p,0.0672-0.0703,0.0413-0.0432");
    println!("standard,720p,0.1512-0.1522,0.0929-0.0935");
    println!("standard,1080p,unverified,unverified");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_to_slug() {
        assert_eq!(Speed::Fast.slug(), "seedance-2.0-fast");
        assert_eq!(Speed::Standard.slug(), "seedance-2.0");
    }

    #[test]
    fn fast_rejects_1080p() {
        let args = GenerateArgs {
            prompt: "x".into(),
            speed: Speed::Fast,
            resolution: Resolution::R1080p,
            duration_seconds: 4,
            aspect_ratio: "16:9".into(),
            images: vec![],
            videos: vec![],
            audios: vec![],
            generate_audio: false,
            seed: -1,
            return_last_frame: false,
            skip_moderation: "false".into(),
            out: Some(PathBuf::from("x.mp4")),
            poll_interval_secs: 1,
            max_wait_secs: 1,
            no_wait: false,
        };
        assert!(validate_generate(&args).is_err());
    }

    #[test]
    fn extracts_nested_media_url() {
        let result = json!({"status":"COMPLETED","output":{"videos":[{"url":"https://example.com/out.mp4?x=1"}]}});
        assert_eq!(
            extract_output_url(&result).as_deref(),
            Some("https://example.com/out.mp4?x=1")
        );
    }

    #[test]
    fn parses_upload_response() {
        let response = json!({"file_urls":["https://images.segmind.com/assets/a.png"]});
        assert_eq!(
            first_uploaded_url(&response).as_deref(),
            Some("https://images.segmind.com/assets/a.png")
        );
    }
}
