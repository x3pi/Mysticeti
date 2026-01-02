#!/usr/bin/env python3
"""
Script để update committee.json với stake từ genesis.json
Tính stake từ delegator_stakes và update threshold
"""
import json
import sys

def update_committee_from_genesis():
    committee_file = '/home/abc/chain-n/Mysticeti/metanode/config/committee.json'
    genesis_file = '/home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/genesis.json'

    # Load files
    try:
        with open(committee_file, 'r') as f:
            committee = json.load(f)
    except FileNotFoundError:
        print(f"❌ Không tìm thấy committee.json: {committee_file}")
        return False

    try:
        with open(genesis_file, 'r') as f:
            genesis = json.load(f)
    except FileNotFoundError:
        print(f"❌ Không tìm thấy genesis.json: {genesis_file}")
        return False

    print("🔄 Update committee.json với stake từ genesis.json...")

    # Calculate total_stake from delegator_stakes
    total_stake = 0
    validators = genesis.get('validators', [])
    authorities = committee.get('authorities', [])

    if len(validators) != len(authorities):
        print(f"⚠️  Số lượng validators ({len(validators)}) != authorities ({len(authorities)})")
        return False

    for i, validator in enumerate(validators):
        # Sum all delegator amounts (in wei)
        validator_stake_wei = 0
        for delegator in validator.get('delegator_stakes', []):
            amount_str = delegator.get('amount', '0')
            try:
                validator_stake_wei += int(amount_str)
            except ValueError:
                print(f"⚠️  Invalid amount: {amount_str}")

        # Convert wei to tokens (divide by 1e18)
        validator_stake_tokens = validator_stake_wei // (10**18)
        total_stake += validator_stake_tokens

        # Update committee authority stake
        authorities[i]['stake'] = validator_stake_tokens
        print(f"  📊 Validator {i} stake: {validator_stake_tokens}")

    # Calculate thresholds
    quorum_threshold = (total_stake * 2) // 3  # 2/3 total_stake
    validity_threshold = total_stake // 3      # 1/3 total_stake

    # Update committee
    committee['total_stake'] = total_stake
    committee['quorum_threshold'] = quorum_threshold
    committee['validity_threshold'] = validity_threshold

    print(f"  📊 Total stake: {total_stake}")
    print(f"  📊 Quorum threshold: {quorum_threshold} (2/3)")
    print(f"  📊 Validity threshold: {validity_threshold} (1/3)")

    # Save updated committee
    with open(committee_file, 'w') as f:
        json.dump(committee, f, indent=2)

    print("✅ Updated committee.json với stake từ delegator_stakes")
    return True

if __name__ == '__main__':
    success = update_committee_from_genesis()
    sys.exit(0 if success else 1)
