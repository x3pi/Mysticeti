#!/usr/bin/env python3
import json

def test_stake_calculation():
    genesis_file = '/home/abc/chain-n/mtn-simple-2025/cmd/simple_chain/genesis.json'
    committee_file = '/home/abc/chain-n/Mysticeti/metanode/config/committee.json'

    # Load genesis
    with open(genesis_file, 'r') as f:
        genesis = json.load(f)

    # Load committee
    with open(committee_file, 'r') as f:
        committee = json.load(f)

    print("=== STAKE CALCULATION TEST ===")

    # Calculate total_stake from delegator_stakes
    total_stake = 0
    validators = genesis.get('validators', [])
    for i, validator in enumerate(validators):
        if i >= len(committee.get('authorities', [])):
            break

        # Sum all delegator amounts (in wei)
        validator_stake_wei = 0
        for delegator in validator.get('delegator_stakes', []):
            amount_str = delegator.get('amount', '0')
            try:
                validator_stake_wei += int(amount_str)
            except ValueError:
                pass

        # Convert wei to tokens (divide by 1e18)
        validator_stake_tokens = validator_stake_wei // (10**18)
        total_stake += validator_stake_tokens

        print(f'Validator {i} stake: {validator_stake_tokens} tokens (from {validator_stake_wei} wei)')

    # Calculate thresholds
    quorum_threshold = (total_stake * 2) // 3
    validity_threshold = total_stake // 3

    print(f'Total stake: {total_stake} tokens')
    print(f'Quorum threshold: {quorum_threshold} (2/3)')
    print(f'Validity threshold: {validity_threshold} (1/3)')

    # Update committee
    committee['total_stake'] = total_stake
    committee['quorum_threshold'] = quorum_threshold
    committee['validity_threshold'] = validity_threshold

    for i, authority in enumerate(committee.get('authorities', [])):
        if i < len(validators):
            validator = validators[i]
            validator_stake_wei = 0
            for delegator in validator.get('delegator_stakes', []):
                amount_str = delegator.get('amount', '0')
                validator_stake_wei += int(amount_str)
            validator_stake_tokens = validator_stake_wei // (10**18)
            authority['stake'] = validator_stake_tokens

    # Save updated committee
    with open(committee_file, 'w') as f:
        json.dump(committee, f, indent=2)

    print(f'Updated committee.json with correct stake values')

if __name__ == '__main__':
    test_stake_calculation()
