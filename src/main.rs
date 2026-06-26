use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose};
use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
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
    after_help = "Examples:\n  seedance generate --prompt \"Neon city at night\" --model mini --resolution 720p --duration-seconds 5 --out video.mp4\n  seedance generate --prompt \"Animate this\" --first-frame ./ref.png --out video.mp4\n  seedance generate --prompt \"Batch job\" --no-wait --pretty\n  seedance task wait --task-id req_123 --out video.mp4"
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
    #[arg(long, value_enum, help = "Seedance model")]
    model: Option<Model>,
    #[arg(long, value_enum, help = "Deprecated alias for --model fast|standard")]
    speed: Option<Speed>,
    #[arg(long, value_enum, default_value_t = Resolution::R720p, help = "Output resolution")]
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
        help = "Starting frame image URL or local path; cannot combine with --image"
    )]
    first_frame: Option<String>,
    #[arg(
        long,
        help = "Ending frame image URL or local path; requires --first-frame"
    )]
    last_frame: Option<String>,
    #[arg(
        long,
        default_value_t = true,
        action = ArgAction::SetTrue,
        help = "Ask Seedance to generate audio when supported"
    )]
    generate_audio: bool,
    #[arg(
        long,
        default_value_t = false,
        action = ArgAction::SetTrue,
        conflicts_with = "generate_audio",
        help = "Disable provider-generated synchronized audio"
    )]
    no_generate_audio: bool,
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
        default_value_t = false,
        action = ArgAction::SetTrue,
        help = "Provider moderation flag passed through to Segmind"
    )]
    skip_moderation: bool,
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
enum Model {
    Mini,
    Fast,
    Standard,
}

impl Model {
    fn slug(self) -> &'static str {
        match self {
            Model::Mini => "seedance-2.0-mini",
            Model::Fast => "seedance-2.0-fast",
            Model::Standard => "seedance-2.0",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Model::Mini => "Seedance 2.0 Mini",
            Model::Fast => "Seedance 2.0 Fast",
            Model::Standard => "Seedance 2.0 Standard",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum Speed {
    Fast,
    Standard,
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
    #[value(name = "4k")]
    #[serde(rename = "4k")]
    R4k,
}

impl Resolution {
    fn as_str(self) -> &'static str {
        match self {
            Resolution::R480p => "480p",
            Resolution::R720p => "720p",
            Resolution::R1080p => "1080p",
            Resolution::R4k => "4k",
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
    let model = resolve_model(&args)?;
    validate_generate(&args, model)?;
    let reference_images = upload_refs(api, &args.images, 9, "images")?;
    let reference_videos = upload_refs(api, &args.videos, 3, "videos")?;
    let reference_audios = upload_refs(api, &args.audios, 3, "audios")?;
    let first_frame_url = upload_optional_ref(api, args.first_frame.as_deref())?;
    let last_frame_url = upload_optional_ref(api, args.last_frame.as_deref())?;
    let mut body = json!({
        "prompt": args.prompt,
        "reference_images": reference_images,
        "reference_videos": reference_videos,
        "reference_audios": reference_audios,
        "duration": args.duration_seconds,
        "resolution": args.resolution.as_str(),
        "aspect_ratio": args.aspect_ratio,
        "generate_audio": args.generate_audio && !args.no_generate_audio,
        "seed": args.seed,
        "return_last_frame": args.return_last_frame,
        "skip_moderation": args.skip_moderation,
    });
    if let Some(first_frame_url) = first_frame_url {
        body["first_frame_url"] = json!(first_frame_url);
    }
    if let Some(last_frame_url) = last_frame_url {
        body["last_frame_url"] = json!(last_frame_url);
    }
    let submit = api.submit(model.slug(), body)?;
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

fn resolve_model(args: &GenerateArgs) -> Result<Model> {
    match (args.model, args.speed) {
        (Some(_), Some(_)) => bail!("use either --model or deprecated --speed, not both"),
        (Some(model), None) => Ok(model),
        (None, Some(Speed::Fast)) => Ok(Model::Fast),
        (None, Some(Speed::Standard)) => Ok(Model::Standard),
        (None, None) => Ok(Model::Mini),
    }
}

fn validate_generate(args: &GenerateArgs, model: Model) -> Result<()> {
    match (model, args.resolution) {
        (Model::Mini | Model::Fast, Resolution::R1080p | Resolution::R4k) => {
            bail!(
                "{} supports 480p or 720p, not {}",
                model.label(),
                args.resolution.as_str()
            )
        }
        _ => {}
    }
    if !matches!(args.duration_seconds, 4 | 5 | 6 | 8 | 10 | 12 | 15) {
        bail!("--duration-seconds must be one of 4, 5, 6, 8, 10, 12, 15");
    }
    if args.first_frame.is_some() && !args.images.is_empty() {
        bail!("--first-frame cannot be combined with --image; use one image-to-video mode");
    }
    if args.last_frame.is_some() && args.first_frame.is_none() {
        bail!("--last-frame requires --first-frame");
    }
    validate_count(&args.images, 9, "images")?;
    validate_count(&args.videos, 3, "videos")?;
    validate_count(&args.audios, 3, "audios")?;
    if !args.no_wait && args.out.is_none() {
        bail!("--out is required unless --no-wait is set");
    }
    Ok(())
}

fn upload_optional_ref(api: &ApiClient, value: Option<&str>) -> Result<Option<String>> {
    value.map(|value| api.upload_one(value)).transpose()
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
    println!("Seedance 2.0 Segmind reference pricing, USD/sec.");
    println!(
        "Multiply text/image-to-video rates by output duration. Video-to-video also depends on reference-video tokens."
    );
    println!();
    print_text_image_rates(
        "mini",
        &[
            (
                "480p",
                ["0.0352", "0.0345", "0.0336", "0.0345", "0.0352", "0.0352"],
            ),
            (
                "720p",
                ["0.0756", "0.0761", "0.0756", "0.0761", "0.0756", "0.0760"],
            ),
        ],
    );
    print_video_rates(
        "mini",
        &[
            ("480p", "~0.045", "0.035-0.065"),
            ("720p", "~0.095", "0.08-0.12"),
        ],
    );
    print_examples(
        "mini",
        &[
            ("Text / Image-to-video", "480p", "16:9", "5s", "0.18"),
            ("Text / Image-to-video", "720p", "16:9", "5s", "0.38"),
            ("Text / Image-to-video", "720p", "16:9", "10s", "0.76"),
            ("Text / Image-to-video", "480p", "16:9", "15s", "0.53"),
            ("Video-to-video", "480p", "16:9", "5s", "~0.22"),
            ("Video-to-video", "720p", "16:9", "5s", "~0.47"),
        ],
    );
    print_text_image_rates(
        "fast",
        &[
            (
                "480p",
                ["0.0562", "0.0553", "0.0538", "0.0553", "0.0562", "0.0562"],
            ),
            (
                "720p",
                ["0.1210", "0.1217", "0.1210", "0.1217", "0.1210", "0.1216"],
            ),
        ],
    );
    print_video_rates(
        "fast",
        &[
            ("480p", "~0.06", "0.055-0.08"),
            ("720p", "~0.13", "0.12-0.17"),
        ],
    );
    print_examples(
        "fast",
        &[
            ("Text / Image-to-video", "480p", "16:9", "5s", "0.28"),
            ("Text / Image-to-video", "720p", "16:9", "5s", "0.60"),
            ("Text / Image-to-video", "720p", "16:9", "10s", "1.21"),
            ("Video-to-video", "480p", "16:9", "5s", "~0.30"),
            ("Video-to-video", "720p", "16:9", "5s", "~0.65"),
        ],
    );
    print_text_image_rates(
        "standard",
        &[
            (
                "480p",
                ["0.0703", "0.0691", "0.0672", "0.0691", "0.0703", "0.0703"],
            ),
            (
                "720p",
                ["0.1512", "0.1522", "0.1512", "0.1522", "0.1512", "0.1519"],
            ),
            ("1080p", ["0.34", "0.34", "0.34", "0.34", "0.34", "0.34"]),
            (
                "4k",
                ["1.3721", "1.3721", "1.3721", "1.3721", "1.3721", "1.3721"],
            ),
        ],
    );
    print_video_rates(
        "standard",
        &[
            ("480p", "~0.09", "0.07-0.13"),
            ("720p", "~0.19", "0.16-0.25"),
            ("1080p", "~0.41", "0.35-0.50"),
        ],
    );
    print_examples(
        "standard",
        &[
            ("Text / Image-to-video", "480p", "16:9", "5s", "0.35"),
            ("Text / Image-to-video", "720p", "16:9", "5s", "0.76"),
            ("Text / Image-to-video", "720p", "16:9", "10s", "1.51"),
            ("Text / Image-to-video", "1080p", "16:9", "5s", "1.70"),
            ("Text / Image-to-video", "4k", "16:9", "5s", "6.86"),
            ("Video-to-video", "480p", "16:9", "5s", "~0.45"),
            ("Video-to-video", "720p", "16:9", "5s", "~0.95"),
        ],
    );
    Ok(())
}

fn print_text_image_rates(model: &str, rows: &[(&str, [&str; 6])]) {
    println!("{model} text/image-to-video per-second rates");
    println!("resolution,16:9,4:3,1:1,3:4,9:16,21:9");
    for (resolution, rates) in rows {
        println!(
            "{resolution},{},{},{},{},{},{}",
            rates[0], rates[1], rates[2], rates[3], rates[4], rates[5]
        );
    }
    println!();
}

fn print_video_rates(model: &str, rows: &[(&str, &str, &str)]) {
    println!("{model} video-to-video typical rates");
    println!("resolution,typical,range");
    for (resolution, typical, range) in rows {
        println!("{resolution},{typical},{range}");
    }
    println!();
}

fn print_examples(model: &str, rows: &[(&str, &str, &str, &str, &str)]) {
    println!("{model} quick cost examples");
    println!("input_type,resolution,aspect_ratio,duration,cost_usd");
    for (input_type, resolution, aspect_ratio, duration, cost) in rows {
        println!("{input_type},{resolution},{aspect_ratio},{duration},{cost}");
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_to_slug() {
        assert_eq!(Model::Mini.slug(), "seedance-2.0-mini");
        assert_eq!(Model::Fast.slug(), "seedance-2.0-fast");
        assert_eq!(Model::Standard.slug(), "seedance-2.0");
    }

    #[test]
    fn default_model_is_mini() {
        let args = base_args();
        assert_eq!(resolve_model(&args).unwrap(), Model::Mini);
    }

    #[test]
    fn speed_alias_resolves_to_model() {
        let mut args = base_args();
        args.speed = Some(Speed::Fast);
        assert_eq!(resolve_model(&args).unwrap(), Model::Fast);
    }

    #[test]
    fn mini_rejects_1080p() {
        let mut args = base_args();
        args.resolution = Resolution::R1080p;
        assert!(validate_generate(&args, Model::Mini).is_err());
    }

    #[test]
    fn standard_accepts_4k() {
        let mut args = base_args();
        args.model = Some(Model::Standard);
        args.resolution = Resolution::R4k;
        assert!(validate_generate(&args, Model::Standard).is_ok());
    }

    #[test]
    fn rejects_unsupported_duration() {
        let mut args = base_args();
        args.duration_seconds = 7;
        assert!(validate_generate(&args, Model::Mini).is_err());
    }

    #[test]
    fn first_frame_rejects_reference_images() {
        let mut args = base_args();
        args.first_frame = Some("https://example.com/start.png".into());
        args.images = vec!["https://example.com/ref.png".into()];
        assert!(validate_generate(&args, Model::Mini).is_err());
    }

    #[test]
    fn last_frame_requires_first_frame() {
        let mut args = base_args();
        args.last_frame = Some("https://example.com/end.png".into());
        assert!(validate_generate(&args, Model::Mini).is_err());
    }

    #[test]
    fn cli_defaults_to_audio_on() {
        let cli =
            Cli::try_parse_from(["seedance", "generate", "--prompt", "x", "--no-wait"]).unwrap();
        let Commands::Generate(args) = cli.command else {
            panic!("expected generate command");
        };
        assert!(args.generate_audio);
        assert!(!args.no_generate_audio);
    }

    #[test]
    fn cli_accepts_audio_off() {
        let cli = Cli::try_parse_from([
            "seedance",
            "generate",
            "--prompt",
            "x",
            "--no-generate-audio",
            "--no-wait",
        ])
        .unwrap();
        let Commands::Generate(args) = cli.command else {
            panic!("expected generate command");
        };
        assert!(args.no_generate_audio);
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

    fn base_args() -> GenerateArgs {
        GenerateArgs {
            prompt: "x".into(),
            model: None,
            speed: None,
            resolution: Resolution::R720p,
            duration_seconds: 5,
            aspect_ratio: "16:9".into(),
            images: vec![],
            videos: vec![],
            audios: vec![],
            first_frame: None,
            last_frame: None,
            generate_audio: true,
            no_generate_audio: false,
            seed: -1,
            return_last_frame: false,
            skip_moderation: false,
            out: Some(PathBuf::from("x.mp4")),
            poll_interval_secs: 1,
            max_wait_secs: 1,
            no_wait: false,
        }
    }
}
