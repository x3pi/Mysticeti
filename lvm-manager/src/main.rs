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
    sudo_password: Option<String>, // Mật khẩu sudo (nếu có)
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

/// Thực thi lệnh với quyền root (sudo).
/// Nếu có password trong config, dùng `sudo -S`.
/// Nếu không, dùng `sudo` thường (hy vọng đã có quyền hoặc NOPASSWD).
fn run_privileged(cmd: &str, args: &[&str], config: &Config) -> Result<()> {
    if let Some(ref pwd) = config.sudo_password {
        // Echo password vào stdin của sudo -S
        let mut child = Command::new("sudo")
            .args(["-S", "-p", "", cmd]) // -p '' để không in prompt
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
                "Lệnh '{}' thất bại với mã lỗi {:?}",
                cmd,
                status.code()
            ))
        }
    } else {
        // Chạy sudo thường
        let status = Command::new("sudo")
            .arg(cmd)
            .args(args)
            .status()
            .context(format!("Không thể chạy lệnh sudo {}", cmd))?;

        if status.success() {
            Ok(())
        } else {
            Err(anyhow!("Lệnh '{}' thất bại", cmd))
        }
    }
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
        if let Err(e) = run_privileged("rm", &["-f", &link_path], &config) {
            println!(
                "⚠️  Không thể xóa symlink bằng sudo: {}. Thử cách khác...",
                e
            );
            let _ = fs::remove_file(&link_path);
        }
        // Xóa file tracking cũ
        let _ = run_privileged("rm", &["-f", &tracking_file], &config);
        println!("✅ Đã xóa symlink và tracking file cũ");
    }

    // 3. Lấy danh sách snapshot hiện có
    let mut snapshots = get_existing_snapshots(&config.vg_name, &config.snap_prefix)?;

    // 4. Xử lý xoay vòng (Rotation) - Giữ tối đa theo config.max_snapshots
    if snapshots.contains(&snap_name) {
        println!("Snapshot {} đã tồn tại. Đang xóa để ghi đè...", snap_name);
        remove_full_snapshot(&config.vg_name, &snap_name, &config.base_path, &config)?;
        snapshots.retain(|x| x != &snap_name);
    }

    if snapshots.len() >= config.max_snapshots {
        snapshots.sort(); // Bản ID nhỏ nhất sẽ đứng đầu
        let to_remove = &snapshots[0];
        println!(
            "Đã đủ {} bản. Đang xóa bản cũ nhất: {}",
            config.max_snapshots, to_remove
        );
        remove_full_snapshot(&config.vg_name, to_remove, &config.base_path, &config)?;
    }

    // 5. Tạo snapshot mới
    println!("Đang tạo snapshot: {}...", snap_name);
    create_lvm_snapshot(&config.vg_name, &config.lv_name, &snap_name, &config)?;

    // 6. Mount snapshot để truy cập dữ liệu
    let mount_point = format!("{}/{}", config.base_path, snap_name);
    fs::create_dir_all(&mount_point)?;

    // 6a. Thử mount với rollback nếu thất bại
    if let Err(e) = mount_readonly(&config.vg_name, &snap_name, &mount_point, &config) {
        println!("❌ Mount thất bại: {}. Đang rollback...", e);
        // Xóa thư mục mount rỗng
        let _ = fs::remove_dir(&mount_point);
        // Xóa LVM snapshot vừa tạo
        let _ = run_privileged(
            "lvremove",
            &["-f", &format!("{}/{}", config.vg_name, snap_name)],
            &config,
        );
        println!("🔄 Đã rollback: xóa thư mục mount và LVM snapshot");
        return Err(e);
    }

    // 6b. VERIFY mount thành công bằng cách kiểm tra thư mục không rỗng
    let entries: Vec<_> = fs::read_dir(&mount_point)
        .context("Không thể đọc mount point")?
        .collect();
    if entries.is_empty() {
        println!(
            "❌ Mount thất bại: thư mục {} rỗng sau mount. Đang rollback...",
            mount_point
        );
        // Umount (có thể không cần nếu mount thất bại, nhưng để chắc chắn)
        let _ = run_privileged("umount", &["-l", &mount_point], &config);
        let _ = fs::remove_dir(&mount_point);
        let _ = run_privileged(
            "lvremove",
            &["-f", &format!("{}/{}", config.vg_name, snap_name)],
            &config,
        );
        println!("🔄 Đã rollback: xóa mount point và LVM snapshot");
        return Err(anyhow!(
            "Mount verification thất bại: thư mục {} rỗng sau mount",
            mount_point
        ));
    }
    println!(
        "✅ Đã verify mount thành công ({} entries trong mount point)",
        entries.len()
    );

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

fn remove_full_snapshot(vg: &str, snap_name: &str, base_path: &str, config: &Config) -> Result<()> {
    let mount_point = format!("{}/{}", base_path, snap_name);

    // Kiểm tra xem symlink 'latest' có đang trỏ đến snapshot này không
    let link_path = format!("{}/latest", base_path);
    if let Some(current_target) = get_current_symlink_target(base_path) {
        if current_target == snap_name {
            println!("⚠️  Symlink 'latest' đang trỏ đến snapshot sắp xóa. Đang xóa symlink...");
            let _ = run_privileged("rm", &["-f", &link_path], config);
            let tracking_file = format!("{}/latest.info", base_path);
            let _ = run_privileged("rm", &["-f", &tracking_file], config);
            println!("✅ Đã xóa symlink và tracking file");
        }
    }

    let _ = run_privileged("umount", &["-l", &mount_point], config);
    let _ = fs::remove_dir_all(&mount_point); // Remove mount point dir, typically doesn't need sudo if owned by user, but strictly speaking generated by root? No, fs::create_dir_all was likely as user.
                                              // Actually, if mount was done as root, the dir might need root to remove? Ideally mount point ownership is preserved.
                                              // If 'umount' succeeds, the dir is just a dir.

    // Use sudo to remove the directory just in case
    let _ = run_privileged("rm", &["-rf", &mount_point], config);

    run_privileged(
        "lvremove",
        &["-f", &format!("{}/{}", vg, snap_name)],
        config,
    )
    .context("Lỗi xóa LV snapshot")
}

fn create_lvm_snapshot(vg: &str, lv: &str, snap_name: &str, config: &Config) -> Result<()> {
    run_privileged(
        "lvcreate",
        &["-s", "-n", snap_name, "-L", "5G", &format!("{}/{}", vg, lv)],
        config,
    )
    .context("Lỗi lệnh lvcreate")
}

fn mount_readonly(vg: &str, snap: &str, path: &str, config: &Config) -> Result<()> {
    run_privileged(
        "mount",
        &["-o", "ro", &format!("/dev/{}/{}", vg, snap), path],
        config,
    )
    .context("Lỗi lệnh mount")
}
