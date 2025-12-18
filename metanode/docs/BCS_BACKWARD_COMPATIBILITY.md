# BCS Backward Compatibility và Migration Strategy

## Vấn đề

BCS (Binary Canonical Serialization) không hỗ trợ backward compatibility khi thêm fields mới vào struct. Khi một node tạo block với các fields mới (`epoch_change_proposal`, `epoch_change_votes`), các nodes khác không thể deserialize nếu chúng chưa có code mới.

## Giải pháp hiện tại (Developer Version)

**Cho developer/testing environment:**
- Re-enable epoch change data trong blocks
- Clear storage khi upgrade
- Đảm bảo tất cả nodes đều có code mới cùng lúc

**Ưu điểm:**
- Đơn giản, dễ implement
- Không cần migration logic phức tạp

**Nhược điểm:**
- Không hỗ trợ rolling upgrade
- Cần downtime để upgrade tất cả nodes

## Giải pháp Production (Dài hạn)

### Option 1: Block Versioning (Recommended)

Sử dụng `BlockV2` với version field và migration logic:

```rust
#[derive(Clone, Deserialize, Serialize)]
pub enum Block {
    V1(BlockV1),  // Old format (no epoch change fields)
    V2(BlockV2),  // New format (with epoch change fields)
}

impl Block {
    pub fn epoch_change_proposal(&self) -> Option<&Vec<u8>> {
        match self {
            Block::V1(_) => None,  // Old blocks don't have epoch change
            Block::V2(block) => block.epoch_change_proposal.as_ref(),
        }
    }
}
```

**Migration:**
- Tất cả nodes upgrade cùng lúc
- Sau upgrade, chỉ tạo `BlockV2`
- Vẫn có thể đọc `BlockV1` cũ

### Option 2: Separate Epoch Change Messages

Không embed epoch change data trong blocks, mà dùng separate message type:

```rust
// Separate message type qua network layer
pub enum ConsensusMessage {
    Block(Block),
    EpochChangeProposal(EpochChangeProposal),
    EpochChangeVote(EpochChangeVote),
}
```

**Ưu điểm:**
- Blocks không thay đổi format
- Epoch change messages độc lập
- Dễ dàng backward compatible

**Nhược điểm:**
- Cần thay đổi network layer
- Phức tạp hơn về implementation

### Option 3: Migration Script

Tạo migration script để convert blocks cũ sang format mới:

```rust
pub fn migrate_block_v1_to_v2(block_v1: BlockV1) -> BlockV2 {
    BlockV2 {
        // Copy all fields
        epoch: block_v1.epoch,
        round: block_v1.round,
        // ...
        // Add default values for new fields
        epoch_change_proposal: None,
        epoch_change_votes: Vec::new(),
    }
}
```

**Migration process:**
1. Stop all nodes
2. Run migration script trên storage
3. Upgrade code
4. Restart nodes

## Recommendation cho Production

**Phase 1 (Current - Developer):**
- ✅ Re-enable epoch change data
- ✅ Clear storage khi upgrade
- ✅ Tất cả nodes upgrade cùng lúc

**Phase 2 (Production - Short term):**
- Implement BlockV2 với versioning
- Migration script cho blocks cũ
- Rolling upgrade support

**Phase 3 (Production - Long term):**
- Separate epoch change messages
- Full backward compatibility
- Zero-downtime upgrades

## Implementation Notes

### Current Implementation

```rust
// In core.rs - Block creation
let (epoch_change_proposal, epoch_change_votes) = 
    crate::epoch_change_provider::get_epoch_change_data();

block.set_epoch_change_proposal(epoch_change_proposal);
block.set_epoch_change_votes(epoch_change_votes);
```

**Requirements:**
- Tất cả nodes phải có code mới
- Storage phải được clear hoặc migrate
- Không hỗ trợ rolling upgrade

### Future Implementation (BlockV2)

```rust
// Always create BlockV2 after upgrade
let block = Block::V2(BlockV2::new(
    // ... fields ...
));

// Blocks cũ vẫn có thể đọc
match block {
    Block::V1(_) => {
        // Old block, no epoch change data
    }
    Block::V2(block) => {
        // New block, may have epoch change data
        if let Some(proposal) = block.epoch_change_proposal.as_ref() {
            // Process epoch change
        }
    }
}
```

## Testing Strategy

### Developer Environment
1. Clear storage
2. Start all nodes với code mới
3. Verify epoch change hoạt động

### Staging Environment
1. Test migration script
2. Test với mixed versions (V1 và V2 blocks)
3. Verify backward compatibility

### Production Environment
1. Deploy migration script
2. Rolling upgrade với BlockV2
3. Monitor cho issues

## Checklist

### Pre-upgrade
- [ ] Backup storage
- [ ] Test migration script
- [ ] Verify all nodes có code mới
- [ ] Plan downtime window

### During upgrade
- [ ] Stop all nodes
- [ ] Run migration script
- [ ] Upgrade code
- [ ] Restart nodes

### Post-upgrade
- [ ] Verify consensus hoạt động
- [ ] Verify epoch change hoạt động
- [ ] Monitor logs cho errors
- [ ] Test epoch transition

## References

- BCS Documentation: https://github.com/diem/bcs
- Block Versioning Pattern: Similar to Sui's approach
- Migration Best Practices: Zero-downtime upgrades

