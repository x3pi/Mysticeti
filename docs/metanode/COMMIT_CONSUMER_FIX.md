# Fix: Commit Consumer Channel Overflow

## Vấn đề

Hệ thống báo lỗi:
- `Failed to send to commit handler, probably due to shutdown: SendError`
- `Failed to send certified blocks: SendError`
- `highest_handled_index=0` - commit handler không xử lý commits

## Nguyên nhân

Trong `src/node.rs`, commit consumer được tạo nhưng receivers (`commit_receiver`, `block_receiver`) bị bỏ qua:

```rust
let (commit_consumer, _commit_receiver, _block_receiver) = CommitConsumerArgs::new(0, 0);
```

Khi không có gì consume từ receivers, channels sẽ bị đầy và gây ra "Failed to send" errors.

## Giải pháp

Spawn tasks để consume từ receivers:

```rust
let (commit_consumer, mut commit_receiver, mut block_receiver) = CommitConsumerArgs::new(0, 0);

// Spawn tasks to consume commits and blocks to prevent channel overflow
tokio::spawn(async move {
    use tracing::info;
    while let Some(subdag) = commit_receiver.recv().await {
        info!(
            "Received commit: index={}, leader={:?}, blocks={}",
            subdag.commit_ref.index,
            subdag.leader,
            subdag.blocks.len()
        );
    }
});

tokio::spawn(async move {
    use tracing::debug;
    while let Some(output) = block_receiver.recv().await {
        debug!("Received {} certified blocks", output.blocks.len());
    }
});
```

## Kết quả

- Channels không còn bị đầy
- Commits được log và xử lý
- `highest_handled_index` sẽ tăng lên
- Không còn "Failed to send" errors

## Build và Test

```bash
cd /home/abc/chain-new/Mysticeti/metanode
cargo build --release --bin metanode
./stop_nodes.sh
./run_nodes.sh
```

Sau đó kiểm tra logs:
```bash
tail -f logs/node_0.log | grep -E "Received commit|quorum_commit_index"
```

