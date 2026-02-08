use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::fs;
use std::process::Command;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "LVM Epoch Snapshot Manager & Download Server"
)]
struct Cli {
    /// Đường dẫn đến file config.toml
    #[arg(short, long, global = true)]
    config: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Tạo snapshot LVM cho một epoch
    Snapshot {
        /// Epoch ID để đặt tên snapshot
        #[arg(short, long)]
        id: i64,
    },
    /// Khởi động HTTP server phục vụ tải snapshot
    Serve {
        /// Port cho HTTP server (default: 8600)
        #[arg(short, long, default_value = "8600")]
        port: u16,

        /// Bind address (default: 0.0.0.0)
        #[arg(short, long, default_value = "0.0.0.0")]
        bind: String,
    },
    /// Legacy mode: tương thích ngược với --id flag cũ
    #[command(hide = true)]
    Legacy {
        #[arg(short, long)]
        id: i64,
    },
}

#[derive(Deserialize, Clone)]
struct Config {
    vg_name: String,
    lv_name: String,
    snap_prefix: String,
    max_snapshots: usize,
    base_path: String,
    sudo_password: Option<String>,
    /// Port cho HTTP server (default: 8600)
    #[serde(default = "default_serve_port")]
    #[allow(dead_code)]
    serve_port: u16,
}

fn default_serve_port() -> u16 {
    8600
}

// ============================================================
// CONFIG FINDER
// ============================================================

fn find_config(config_arg: &Option<String>) -> Result<String> {
    if let Some(path) = config_arg {
        if std::path::Path::new(path).exists() {
            println!("📁 Config: {}", path);
            return fs::read_to_string(path)
                .context(format!("Không thể đọc file config: {}", path));
        }
        return Err(anyhow!("File config không tồn tại: {}", path));
    }

    if std::path::Path::new("config.toml").exists() {
        println!("📁 Config: ./config.toml");
        return fs::read_to_string("config.toml").context("Không thể đọc config.toml");
    }

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let config_in_root = exe_dir.join("../../config.toml");
            if config_in_root.exists() {
                return fs::read_to_string(&config_in_root)
                    .context(format!("Không thể đọc: {:?}", config_in_root));
            }
            let config_in_exe_dir = exe_dir.join("config.toml");
            if config_in_exe_dir.exists() {
                return fs::read_to_string(&config_in_exe_dir)
                    .context(format!("Không thể đọc: {:?}", config_in_exe_dir));
            }
        }
    }

    Err(anyhow!("Không tìm thấy config.toml"))
}

// ============================================================
// PRIVILEGED COMMAND EXECUTION
// ============================================================

fn run_privileged(cmd: &str, args: &[&str], config: &Config) -> Result<()> {
    if let Some(ref pwd) = config.sudo_password {
        let mut child = Command::new("sudo")
            .args(["-S", "-p", "", cmd])
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .context(format!("Không thể khởi chạy sudo {}", cmd))?;

        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(pwd.as_bytes())?;
            stdin.write_all(b"\n")?;
        }

        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(anyhow!(
                "Lệnh '{}' thất bại (exit {:?})",
                cmd,
                status.code()
            ))
        }
    } else {
        let status = Command::new("sudo")
            .arg(cmd)
            .args(args)
            .status()
            .context(format!("Không thể chạy sudo {}", cmd))?;
        if status.success() {
            Ok(())
        } else {
            Err(anyhow!("Lệnh '{}' thất bại", cmd))
        }
    }
}

// ============================================================
// SNAPSHOT OPERATIONS
// ============================================================

fn create_snapshot(config: &Config, epoch_id: i64) -> Result<()> {
    let snap_name = format!("{}_{:06}", config.snap_prefix, epoch_id);
    println!("📸 Tạo snapshot cho epoch {} ({})", epoch_id, snap_name);

    let mut snapshots = get_existing_snapshots(&config.vg_name, &config.snap_prefix)?;
    println!("📋 Snapshot hiện có: {:?}", snapshots);

    // Xóa nếu đã tồn tại
    if snapshots.contains(&snap_name) {
        println!("⚠️  {} đã tồn tại. Xóa để ghi đè...", snap_name);
        remove_snapshot(&config.vg_name, &snap_name, &config.base_path, config)?;
        snapshots.retain(|x| x != &snap_name);
    }

    // Xoay vòng: giữ tối đa max_snapshots bản
    if snapshots.len() >= config.max_snapshots {
        snapshots.sort();
        let to_remove = snapshots[0].clone();
        println!(
            "🔄 Đã đủ {} bản. Xóa cũ nhất: {}",
            config.max_snapshots, to_remove
        );
        remove_snapshot(&config.vg_name, &to_remove, &config.base_path, config)?;
    }

    // Tạo snapshot LVM mới
    println!("🔧 Đang tạo LVM snapshot: {}...", snap_name);
    run_privileged(
        "lvcreate",
        &[
            "-s",
            "-n",
            &snap_name,
            "-L",
            "5G",
            &format!("{}/{}", config.vg_name, config.lv_name),
        ],
        config,
    )
    .context("Lỗi lvcreate")?;

    // Mount read-only
    let mount_point = format!("{}/{}", config.base_path, snap_name);
    fs::create_dir_all(&mount_point)?;

    if let Err(e) = run_privileged(
        "mount",
        &[
            "-o",
            "ro",
            &format!("/dev/{}/{}", config.vg_name, snap_name),
            &mount_point,
        ],
        config,
    ) {
        println!("❌ Mount thất bại: {}. Rollback...", e);
        let _ = fs::remove_dir(&mount_point);
        let _ = run_privileged(
            "lvremove",
            &["-f", &format!("{}/{}", config.vg_name, snap_name)],
            config,
        );
        return Err(e);
    }

    // Verify
    let entries: Vec<_> = fs::read_dir(&mount_point)
        .context("Không thể đọc mount point")?
        .collect();
    if entries.is_empty() {
        println!("❌ Mount rỗng. Rollback...");
        let _ = run_privileged("umount", &["-l", &mount_point], config);
        let _ = fs::remove_dir(&mount_point);
        let _ = run_privileged(
            "lvremove",
            &["-f", &format!("{}/{}", config.vg_name, snap_name)],
            config,
        );
        return Err(anyhow!("Mount thất bại: thư mục rỗng"));
    }

    println!(
        "✅ Snapshot epoch {} thành công! ({} entries tại {})",
        epoch_id,
        entries.len(),
        mount_point
    );

    let final_snaps = get_existing_snapshots(&config.vg_name, &config.snap_prefix)?;
    println!("📋 Danh sách snapshot: {:?}", final_snaps);
    Ok(())
}

fn get_existing_snapshots(vg: &str, prefix: &str) -> Result<Vec<String>> {
    let output = Command::new("lvs")
        .args(["--noheadings", "-o", "lv_name", vg])
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| l.starts_with(prefix))
        .collect())
}

fn remove_snapshot(vg: &str, snap_name: &str, base_path: &str, config: &Config) -> Result<()> {
    let mount_point = format!("{}/{}", base_path, snap_name);
    let _ = run_privileged("umount", &["-l", &mount_point], config);
    let _ = run_privileged("rm", &["-rf", &mount_point], config);
    run_privileged(
        "lvremove",
        &["-f", &format!("{}/{}", vg, snap_name)],
        config,
    )
    .context("Lỗi xóa LV snapshot")
}

// ============================================================
// HTTP FILE SERVER - Phục vụ tải snapshot
// ============================================================

async fn run_server(config: &Config, bind: &str, port: u16) -> Result<()> {
    use axum::extract::State;
    use axum::response::{Html, IntoResponse};
    use axum::Router;
    use tower_http::services::ServeDir;

    let base_path = config.base_path.clone();
    let snap_prefix = config.snap_prefix.clone();

    println!("╔═══════════════════════════════════════════════════════════╗");
    println!("║       SNAPSHOT DOWNLOAD SERVER                           ║");
    println!("╚═══════════════════════════════════════════════════════════╝");
    println!();
    println!("📂 Serving snapshots from: {}", base_path);
    println!("🌐 Server: http://{}:{}", bind, port);
    println!("📋 Index page: http://{}:{}/", bind, port);
    println!(
        "📦 Download:   http://{}:{}/snap_id_XXXXXX/path/to/file",
        bind, port
    );
    println!();
    println!("💡 Hỗ trợ:");
    println!("   ✅ HTTP Range requests (tiếp tục tải nếu bị lỗi)");
    println!("   ✅ Streaming (không giới hạn dung lượng file)");
    println!("   ✅ Tải đa luồng / nhiều kết nối đồng thời");
    println!();
    println!("📥 Ví dụ tải bằng wget (hỗ trợ resume):");
    println!("   wget -c -r -np http://{}:{}/snap_id_000144/", bind, port);
    println!();
    println!("📥 Ví dụ tải bằng aria2c (đa luồng, resume):");
    println!(
        "   aria2c -x 16 -s 16 -c http://{}:{}/snap_id_000144/file.db",
        bind, port
    );
    println!();

    // Shared state for index page
    #[derive(Clone)]
    struct AppState {
        base_path: String,
        snap_prefix: String,
    }

    let state = AppState {
        base_path: base_path.clone(),
        snap_prefix: snap_prefix.clone(),
    };

    // Index page handler - lists available snapshots
    async fn index_handler(State(state): State<AppState>) -> impl IntoResponse {
        let mut snapshots = Vec::new();

        if let Ok(entries) = fs::read_dir(&state.base_path) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(&state.snap_prefix) && entry.path().is_dir() {
                    // Get total size of snapshot directory
                    let size = get_dir_size_human(&entry.path());
                    snapshots.push((name, size));
                }
            }
        }
        snapshots.sort();

        let mut html = String::from(
            r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>Blockchain Snapshot Server</title>
    <style>
        body { font-family: 'Segoe UI', monospace; background: #0d1117; color: #c9d1d9; padding: 40px; }
        h1 { color: #58a6ff; border-bottom: 1px solid #30363d; padding-bottom: 16px; }
        .info { color: #8b949e; margin-bottom: 24px; }
        table { border-collapse: collapse; width: 100%; }
        th { text-align: left; color: #58a6ff; border-bottom: 2px solid #30363d; padding: 12px 16px; }
        td { padding: 12px 16px; border-bottom: 1px solid #21262d; }
        a { color: #58a6ff; text-decoration: none; font-weight: bold; font-size: 1.1em; }
        a:hover { text-decoration: underline; color: #79c0ff; }
        .size { color: #f0883e; font-weight: bold; }
        .badge { background: #238636; color: white; padding: 2px 8px; border-radius: 12px; font-size: 0.8em; margin-left: 8px; }
        .help { background: #161b22; border: 1px solid #30363d; border-radius: 6px; padding: 16px; margin-top: 24px; }
        .help h3 { color: #58a6ff; margin-top: 0; }
        code { background: #21262d; padding: 2px 6px; border-radius: 4px; color: #f0883e; }
        pre { background: #161b22; padding: 12px; border-radius: 6px; overflow-x: auto; color: #c9d1d9; }
    </style>
</head>
<body>
    <h1>📦 Blockchain Snapshot Server</h1>
    <div class="info">Phục vụ snapshot blockchain cho đồng bộ node mới. Hỗ trợ resume tải khi bị lỗi.</div>
    <table>
        <tr><th>Snapshot</th><th>Dung lượng</th><th>Hành động</th></tr>
"#,
        );

        if snapshots.is_empty() {
            html.push_str(r#"<tr><td colspan="3" style="text-align:center;color:#8b949e;">Chưa có snapshot nào</td></tr>"#);
        } else {
            let latest = snapshots.last().map(|(n, _)| n.clone()).unwrap_or_default();
            for (name, size) in &snapshots {
                let badge = if *name == latest {
                    r#"<span class="badge">latest</span>"#
                } else {
                    ""
                };
                html.push_str(&format!(
                    r#"<tr><td><a href="/{name}/">{name}</a>{badge}</td><td class="size">{size}</td><td><a href="/{name}/">📂 Browse</a></td></tr>"#,
                ));
            }
        }

        html.push_str(r#"
    </table>
    <div class="help">
        <h3>📥 Cách tải snapshot</h3>
        <p><strong>wget</strong> (hỗ trợ resume với <code>-c</code>):</p>
        <pre>wget -c -r -np -nH --cut-dirs=1 http://&lt;server&gt;:&lt;port&gt;/&lt;snapshot&gt;/</pre>
        <p><strong>aria2c</strong> (đa luồng, resume, nhanh nhất cho file lớn):</p>
        <pre>aria2c -x 16 -s 16 -c http://&lt;server&gt;:&lt;port&gt;/&lt;snapshot&gt;/path/to/large_file.db</pre>
        <p><strong>rsync</strong> (incremental sync):</p>
        <pre>rsync -avz --progress rsync://&lt;server&gt;/snapshots/ /local/path/</pre>
        <p><strong>curl</strong> (resume với <code>-C -</code>):</p>
        <pre>curl -C - -O http://&lt;server&gt;:&lt;port&gt;/&lt;snapshot&gt;/path/to/file</pre>
    </div>
</body>
</html>"#);

        Html(html)
    }

    fn get_dir_size_human(path: &std::path::Path) -> String {
        // Use du command for fast directory size
        if let Ok(output) = Command::new("du")
            .args(["-sh", &path.to_string_lossy()])
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(size) = stdout.split_whitespace().next() {
                return size.to_string();
            }
        }
        "N/A".to_string()
    }

    // Build router:
    // - GET /  → index page with snapshot listing
    // - GET /snap_id_xxx/... → serve files with range support (streaming)
    let app = Router::new()
        .route("/", axum::routing::get(index_handler))
        .with_state(state)
        .fallback_service(ServeDir::new(&base_path).append_index_html_on_directories(false));

    let addr = format!("{}:{}", bind, port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .context(format!("Không thể bind tại {}", addr))?;

    println!("🚀 Server đang chạy tại http://{}", addr);
    println!("   Nhấn Ctrl+C để dừng");

    axum::serve(listener, app).await.context("Server error")?;

    Ok(())
}

// ============================================================
// MAIN - Subcommand dispatch
// ============================================================

fn main() -> Result<()> {
    // Detect if user is using subcommand format (snapshot/serve) or legacy format (--id)
    let args: Vec<String> = std::env::args().collect();
    let has_subcommand = args.iter().any(|a| a == "snapshot" || a == "serve");

    if has_subcommand {
        // New subcommand format
        let cli = Cli::parse();
        let config_content = find_config(&cli.config)?;
        let config: Config = toml::from_str(&config_content)?;

        match cli.command {
            Commands::Snapshot { id } | Commands::Legacy { id } => create_snapshot(&config, id),
            Commands::Serve { port, bind } => {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .context("Không thể tạo tokio runtime")?;
                rt.block_on(run_server(&config, &bind, port))
            }
        }
    } else {
        // Legacy mode: lvm-snap-rsync --id 144
        let legacy = LegacyArgs::parse();
        let config_content = find_config(&legacy.config)?;
        let config: Config = toml::from_str(&config_content)?;
        create_snapshot(&config, legacy.id)
    }
}

/// Legacy argument parser for backward compatibility
/// Supports: lvm-snap-rsync --id 144
#[derive(Parser, Debug)]
#[command(author, version, about = "LVM Epoch Snapshot Manager")]
struct LegacyArgs {
    #[arg(short, long)]
    id: i64,
    #[arg(short, long)]
    config: Option<String>,
}
