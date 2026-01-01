use std::process::Command;
use std::fs;
use std::os::unix::fs::symlink;
use serde::Deserialize;
use anyhow::{Result, Context, anyhow};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about = "LVM Snapshot Manager")]
struct Args {
    /// Số ID tăng dần (0, 1, 2...) để đặt tên snapshot
    #[arg(short, long)]
    id: i64,
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

fn main() -> Result<()> {
    let args = Args::parse();

    // 1. Đọc cấu hình
    let config_content = fs::read_to_string("config.toml")
        .context("Không tìm thấy file config.toml")?;
    let config: Config = toml::from_str(&config_content)?;

    // Tên snapshot mới dựa trên ID truyền vào
    let snap_name = format!("{}_{:06}", config.snap_prefix, args.id);
    println!(">>> Kích hoạt tạo Snapshot với ID: {}", args.id);

    // 2. Lấy danh sách snapshot hiện có
    let mut snapshots = get_existing_snapshots(&config.vg_name, &config.snap_prefix)?;

    // 3. Xử lý xoay vòng (Rotation) - Giữ tối đa theo config.max_snapshots
    if snapshots.contains(&snap_name) {
        println!("Snapshot {} đã tồn tại. Đang xóa để ghi đè...", snap_name);
        remove_full_snapshot(&config.vg_name, &snap_name, &config.base_path)?;
        snapshots.retain(|x| x != &snap_name);
    }

    if snapshots.len() >= config.max_snapshots {
        snapshots.sort(); // Bản ID nhỏ nhất sẽ đứng đầu
        let to_remove = &snapshots[0];
        println!("Đã đủ {} bản. Đang xóa bản cũ nhất: {}", config.max_snapshots, to_remove);
        remove_full_snapshot(&config.vg_name, to_remove, &config.base_path)?;
    }

    // 4. Tạo snapshot mới
    println!("Đang tạo snapshot: {}...", snap_name);
    create_lvm_snapshot(&config.vg_name, &config.lv_name, &snap_name)?;

    // 5. Mount snapshot để truy cập dữ liệu
    let mount_point = format!("{}/{}", config.base_path, snap_name);
    fs::create_dir_all(&mount_point)?;
    mount_readonly(&config.vg_name, &snap_name, &mount_point)?;

    // 6. Cập nhật symlink 'latest' trỏ vào THƯ MỤC CON thay vì toàn bộ ổ đĩa
    let target_with_subdir = format!("{}/{}", mount_point, config.share_subdir);
    update_latest_symlink(&target_with_subdir, &config.base_path)?;

    println!("--- HOÀN TẤT: {} (thư mục {}) sẵn sàng chia sẻ ---", snap_name, config.share_subdir);
    Ok(())
}

fn get_existing_snapshots(vg: &str, prefix: &str) -> Result<Vec<String>> {
    let output = Command::new("lvs")
        .args(["--noheadings", "-o", "lv_name", vg])
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let snaps: Vec<String> = stdout.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| l.starts_with(prefix))
        .collect();
    Ok(snaps)
}

fn remove_full_snapshot(vg: &str, snap_name: &str, base_path: &str) -> Result<()> {
    let mount_point = format!("{}/{}", base_path, snap_name);
    let _ = Command::new("umount").arg("-l").arg(&mount_point).status();
    let _ = fs::remove_dir_all(&mount_point);
    let status = Command::new("lvremove")
        .args(["-f", &format!("{}/{}", vg, snap_name)])
        .status()?;
    if status.success() { Ok(()) } else { Err(anyhow!("Lỗi xóa LV snapshot")) }
}

fn create_lvm_snapshot(vg: &str, lv: &str, snap_name: &str) -> Result<()> {
    let status = Command::new("lvcreate")
        .args(["-s", "-n", snap_name, "-L", "1G", &format!("{}/{}", vg, lv)])
        .status()?;
    if status.success() { Ok(()) } else { Err(anyhow!("Lỗi lệnh lvcreate")) }
}

fn mount_readonly(vg: &str, snap: &str, path: &str) -> Result<()> {
    let status = Command::new("mount")
        .args(["-o", "ro", &format!("/dev/{}/{}", vg, snap), path])
        .status()?;
    if status.success() { Ok(()) } else { Err(anyhow!("Lỗi lệnh mount")) }
}

fn update_latest_symlink(target: &str, base_path: &str) -> Result<()> {
    let link_path = format!("{}/latest", base_path);
    if fs::metadata(&link_path).is_ok() {
        fs::remove_file(&link_path)?;
    }
    symlink(target, link_path).context("Lỗi tạo symlink")
}