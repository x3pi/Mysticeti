use anyhow::{anyhow, Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::fs;
use std::os::unix::fs::symlink;
use std::process::Command;

#[derive(Parser, Debug)]
#[command(author, version, about = "LVM Snapshot Manager")]
struct Args {
    /// Số ID tăng dần (0, 1, 2...) để đặt tên snapshot
    #[arg(short, long)]
    id: i64,

    /// Đường dẫn đến file config.toml (mặc định: tìm ở thư mục hiện tại hoặc thư mục binary)
    #[arg(short, long)]
    config: Option<String>,
}

#[derive(Deserialize)]
struct Config {
    vg_name: String,
    lv_name: String,
    snap_prefix: String,
    max_snapshots: usize,
    base_path: String,
    share_subdir: String, // Thêm trường này để xác định thư mục con cần share
}

/// Tìm file config.toml theo thứ tự ưu tiên:
/// 1. Đường dẫn được chỉ định qua --config
/// 2. Thư mục hiện tại (./config.toml)
/// 3. Cùng thư mục với binary executable
fn find_config(config_arg: &Option<String>) -> Result<String> {
    // 1. Nếu được chỉ định qua argument
    if let Some(path) = config_arg {
        if std::path::Path::new(path).exists() {
            println!("📁 Sử dụng config từ argument: {}", path);
            return fs::read_to_string(path)
                .context(format!("Không thể đọc file config: {}", path));
        }
        return Err(anyhow!("File config không tồn tại: {}", path));
    }

    // 2. Thử tìm ở thư mục hiện tại
    if std::path::Path::new("config.toml").exists() {
        println!("📁 Sử dụng config từ thư mục hiện tại");
        return fs::read_to_string("config.toml").context("Không thể đọc file config.toml");
    }

    // 3. Tìm ở thư mục chứa binary executable
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            // Thử ở thư mục parent của target/release (tức là lvm-manager root)
            let config_in_root = exe_dir.join("../../config.toml");
            if config_in_root.exists() {
                println!("📁 Sử dụng config từ thư mục gốc: {:?}", config_in_root);
                return fs::read_to_string(&config_in_root)
                    .context(format!("Không thể đọc file config: {:?}", config_in_root));
            }

            // Thử ở cùng thư mục với binary
            let config_in_exe_dir = exe_dir.join("config.toml");
            if config_in_exe_dir.exists() {
                println!(
                    "📁 Sử dụng config từ thư mục binary: {:?}",
                    config_in_exe_dir
                );
                return fs::read_to_string(&config_in_exe_dir).context(format!(
                    "Không thể đọc file config: {:?}",
                    config_in_exe_dir
                ));
            }
        }
    }

    Err(anyhow!(
        "Không tìm thấy file config.toml. Vui lòng chỉ định đường dẫn qua --config hoặc đặt file ở thư mục hiện tại."
    ))
}

fn main() -> Result<()> {
    let args = Args::parse();

    // 1. Đọc cấu hình - tìm ở nhiều vị trí
    let config_content = find_config(&args.config)?;
    let config: Config = toml::from_str(&config_content)?;

    // Tên snapshot mới dựa trên ID truyền vào
    let snap_name = format!("{}_{:06}", config.snap_prefix, args.id);
    println!(">>> Kích hoạt tạo Snapshot với ID: {}", args.id);

    // 2. CRITICAL: Xóa symlink 'latest' TRƯỚC KHI xóa snapshot cũ
    // Điều này đảm bảo symlink không bao giờ trỏ vào snapshot đã bị xóa (stale/broken)
    let link_path = format!("{}/latest", config.base_path);
    let tracking_file = format!("{}/latest.info", config.base_path);

    if fs::symlink_metadata(&link_path).is_ok() {
        println!("🔄 Bước đầu tiên: Xóa symlink 'latest' cũ trước khi rotation...");
        if let Err(e) = Command::new("sudo")
            .arg("rm")
            .arg("-f")
            .arg(&link_path)
            .status()
        {
            println!(
                "⚠️  Không thể xóa symlink bằng sudo: {}. Thử cách khác...",
                e
            );
            let _ = fs::remove_file(&link_path);
        }
        // Xóa file tracking cũ
        let _ = Command::new("sudo")
            .arg("rm")
            .arg("-f")
            .arg(&tracking_file)
            .status();
        println!("✅ Đã xóa symlink và tracking file cũ");
    }

    // 3. Lấy danh sách snapshot hiện có
    let mut snapshots = get_existing_snapshots(&config.vg_name, &config.snap_prefix)?;

    // 4. Xử lý xoay vòng (Rotation) - Giữ tối đa theo config.max_snapshots
    if snapshots.contains(&snap_name) {
        println!("Snapshot {} đã tồn tại. Đang xóa để ghi đè...", snap_name);
        remove_full_snapshot(&config.vg_name, &snap_name, &config.base_path)?;
        snapshots.retain(|x| x != &snap_name);
    }

    if snapshots.len() >= config.max_snapshots {
        snapshots.sort(); // Bản ID nhỏ nhất sẽ đứng đầu
        let to_remove = &snapshots[0];
        println!(
            "Đã đủ {} bản. Đang xóa bản cũ nhất: {}",
            config.max_snapshots, to_remove
        );
        remove_full_snapshot(&config.vg_name, to_remove, &config.base_path)?;
    }

    // 5. Tạo snapshot mới
    println!("Đang tạo snapshot: {}...", snap_name);
    create_lvm_snapshot(&config.vg_name, &config.lv_name, &snap_name)?;

    // 6. Mount snapshot để truy cập dữ liệu
    let mount_point = format!("{}/{}", config.base_path, snap_name);
    fs::create_dir_all(&mount_point)?;
    mount_readonly(&config.vg_name, &snap_name, &mount_point)?;

    // 7. Tạo symlink 'latest' MỚI trỏ vào THƯ MỤC CON
    // (symlink cũ đã được xóa ở bước 2 trước khi rotation)
    // Handle both absolute and relative paths for share_subdir
    let target_with_subdir = if config.share_subdir.starts_with('/') {
        format!("{}{}", mount_point, config.share_subdir)
    } else {
        format!("{}/{}", mount_point, config.share_subdir)
    };
    println!(
        "Đang tạo symlink latest: {} -> {}",
        link_path, target_with_subdir
    );

    // Double-check that target exists before creating symlink
    if !std::path::Path::new(&target_with_subdir).exists() {
        return Err(anyhow!(
            "❌ Target directory không tồn tại: {}",
            target_with_subdir
        ));
    }

    // Symlink đã được xóa ở bước 2, giờ tạo mới

    symlink(&target_with_subdir, &link_path).context("Lỗi tạo symlink latest")?;
    println!("✅ Tạo symlink latest thành công");

    // Tạo file tracking để biết symlink latest đang trỏ tới đâu
    let tracking_file = format!("{}/latest.info", config.base_path);
    let current_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let tracking_content = format!(
        "# LVM Snapshot Latest Symlink Tracking\n\
         # Generated at: {} (Unix timestamp)\n\
         # Snapshot Name: {}\n\
         # Symlink Path: {}\n\
         # Target Path: {}\n\
         # Mount Point: {}\n\
         # Share Subdir: {}\n\
         \n\
         snapshot_name={}\n\
         symlink_path={}\n\
         target_path={}\n\
         mount_point={}\n\
         share_subdir={}\n\
         created_at={}\n",
        current_time,
        snap_name,
        link_path,
        target_with_subdir,
        mount_point,
        config.share_subdir,
        snap_name,
        link_path,
        target_with_subdir,
        mount_point,
        config.share_subdir,
        current_time
    );

    fs::write(&tracking_file, tracking_content)
        .context(format!("Lỗi ghi file tracking: {}", tracking_file))?;
    println!("📋 Đã tạo file tracking: {}", tracking_file);

    println!(
        "--- HOÀN TẤT: {} (thư mục {}) sẵn sàng chia sẻ ---",
        snap_name, config.share_subdir
    );
    Ok(())
}

/// Lấy tên snapshot mà symlink 'latest' đang trỏ tới
/// Trả về None nếu symlink không tồn tại hoặc không thể đọc
fn get_current_symlink_target(base_path: &str) -> Option<String> {
    let link_path = format!("{}/latest", base_path);
    if let Ok(target) = fs::read_link(&link_path) {
        // Extract snapshot name from target path
        // e.g., /mnt/lvm_public/snap_id_000004/... -> snap_id_000004
        if let Some(path_str) = target.to_str() {
            for component in path_str.split('/') {
                if component.starts_with("snap_id_") {
                    return Some(component.to_string());
                }
            }
        }
    }
    None
}

fn get_existing_snapshots(vg: &str, prefix: &str) -> Result<Vec<String>> {
    let output = Command::new("lvs")
        .args(["--noheadings", "-o", "lv_name", vg])
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let snaps: Vec<String> = stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| l.starts_with(prefix))
        .collect();
    Ok(snaps)
}

fn remove_full_snapshot(vg: &str, snap_name: &str, base_path: &str) -> Result<()> {
    let mount_point = format!("{}/{}", base_path, snap_name);

    // Kiểm tra xem symlink 'latest' có đang trỏ đến snapshot này không
    let link_path = format!("{}/latest", base_path);
    if let Some(current_target) = get_current_symlink_target(base_path) {
        if current_target == snap_name {
            println!("⚠️  Symlink 'latest' đang trỏ đến snapshot sắp xóa. Đang xóa symlink...");
            let _ = fs::remove_file(&link_path);
            let tracking_file = format!("{}/latest.info", base_path);
            let _ = fs::remove_file(&tracking_file);
            println!("✅ Đã xóa symlink và tracking file");
        }
    }

    let _ = Command::new("umount").arg("-l").arg(&mount_point).status();
    let _ = fs::remove_dir_all(&mount_point);
    let status = Command::new("lvremove")
        .args(["-f", &format!("{}/{}", vg, snap_name)])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("Lỗi xóa LV snapshot"))
    }
}

fn create_lvm_snapshot(vg: &str, lv: &str, snap_name: &str) -> Result<()> {
    let status = Command::new("lvcreate")
        .args(["-s", "-n", snap_name, "-L", "1G", &format!("{}/{}", vg, lv)])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("Lỗi lệnh lvcreate"))
    }
}

fn mount_readonly(vg: &str, snap: &str, path: &str) -> Result<()> {
    let status = Command::new("mount")
        .args(["-o", "ro", &format!("/dev/{}/{}", vg, snap), path])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("Lỗi lệnh mount"))
    }
}
