use consensus_core::SystemTransaction; fn main() { let data = vec![1,2,3]; let _ = SystemTransaction::from_bytes(&data); }
