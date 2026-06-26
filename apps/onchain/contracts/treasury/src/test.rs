use super::*;
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{token, Address, BytesN, Env};

fn request_id(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0; 32])
}

#[test]
fn test_treasury_streaming() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let token_admin = Address::generate(&env);

    // Deploy token
    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_client = token::TokenClient::new(&env, &token_id.address());
    let token_admin_client = token::StellarAssetClient::new(&env, &token_id.address());

    // Deploy treasury
    let treasury_id = env.register(TreasuryContract, ());
    let treasury_client = TreasuryContractClient::new(&env, &treasury_id);

    // Initialize
    treasury_client.initialize(&admin, &token_id.address());

    // Mint tokens to admin
    let amount = 1000i128;
    token_admin_client.mint(&admin, &amount);

    // Allocate budget
    let start_time = 1000u64;
    let duration = 1000u64;
    env.ledger().set_timestamp(start_time);

    treasury_client.allocate_budget(
        &admin,
        &beneficiary,
        &amount,
        &start_time,
        &duration,
        &request_id(&env),
    );

    // Check unlocked at start_time (should be 0)
    assert_eq!(treasury_client.get_unlocked(&beneficiary), 0);

    // Move time forward by 500 seconds (half duration)
    env.ledger().set_timestamp(start_time + 500);
    assert_eq!(treasury_client.get_unlocked(&beneficiary), 500);

    // Claim half
    let claimed = treasury_client.claim(&beneficiary);
    assert_eq!(claimed, 500);
    assert_eq!(token_client.balance(&beneficiary), 500);

    // Check unlocked again (should be 0 now since we just claimed)
    assert_eq!(treasury_client.get_unlocked(&beneficiary), 0);

    // Move time forward to end
    env.ledger().set_timestamp(start_time + 1000);
    assert_eq!(treasury_client.get_unlocked(&beneficiary), 500);

    // Claim rest
    treasury_client.claim(&beneficiary);
    assert_eq!(token_client.balance(&beneficiary), 1000);
}

#[test]
fn test_allocate_budget_duplicate_request_id() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let token_admin = Address::generate(&env);

    // Deploy token
    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_admin_client = token::StellarAssetClient::new(&env, &token_id.address());

    // Deploy treasury
    let treasury_id = env.register(TreasuryContract, ());
    let treasury_client = TreasuryContractClient::new(&env, &treasury_id);

    // Initialize
    treasury_client.initialize(&admin, &token_id.address());

    // Mint tokens to admin
    let amount = 1000i128;
    token_admin_client.mint(&admin, &amount);

    let start_time = 1000u64;
    let duration = 1000u64;
    env.ledger().set_timestamp(start_time);

    // First allocation should succeed
    treasury_client.allocate_budget(
        &admin,
        &beneficiary,
        &amount,
        &start_time,
        &duration,
        &request_id(&env),
    );

    // Second allocation with same request_id should fail
    let result = treasury_client.try_allocate_budget(
        &admin,
        &beneficiary,
        &amount,
        &start_time,
        &duration,
        &request_id(&env),
    );
    assert_eq!(result, Err(Ok(TreasuryError::AlreadyExecuted)));
}

#[test]
fn test_rotate_beneficiary_before_claims() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let old_beneficiary = Address::generate(&env);
    let new_beneficiary = Address::generate(&env);
    let token_admin = Address::generate(&env);

    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_client = token::TokenClient::new(&env, &token_id.address());
    let token_admin_client = token::StellarAssetClient::new(&env, &token_id.address());

    let treasury_id = env.register(TreasuryContract, ());
    let treasury_client = TreasuryContractClient::new(&env, &treasury_id);

    treasury_client.initialize(&admin, &token_id.address());

    let amount = 1000i128;
    token_admin_client.mint(&admin, &amount);

    let start_time = 1000u64;
    let duration = 1000u64;
    env.ledger().set_timestamp(start_time);

    treasury_client.allocate_budget(&admin, &old_beneficiary, &amount, &start_time, &duration, &request_id(&env));

    env.ledger().set_timestamp(start_time + 500);

    treasury_client.rotate_beneficiary(&admin, &old_beneficiary, &new_beneficiary);

    assert_eq!(
        treasury_client.try_get_unlocked(&old_beneficiary),
        Err(Ok(TreasuryError::StreamNotFound))
    );

    assert_eq!(treasury_client.get_unlocked(&new_beneficiary), 500);

    env.ledger().set_timestamp(start_time + 1000);
    assert_eq!(treasury_client.get_unlocked(&new_beneficiary), 1000);

    let claimed = treasury_client.claim(&new_beneficiary);
    assert_eq!(claimed, 1000);
    assert_eq!(token_client.balance(&new_beneficiary), 1000);
}

#[test]
fn test_rotate_beneficiary_after_partial_claims() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let old_beneficiary = Address::generate(&env);
    let new_beneficiary = Address::generate(&env);
    let token_admin = Address::generate(&env);

    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_client = token::TokenClient::new(&env, &token_id.address());
    let token_admin_client = token::StellarAssetClient::new(&env, &token_id.address());

    let treasury_id = env.register(TreasuryContract, ());
    let treasury_client = TreasuryContractClient::new(&env, &treasury_id);

    treasury_client.initialize(&admin, &token_id.address());

    let amount = 1000i128;
    token_admin_client.mint(&admin, &amount);

    let start_time = 1000u64;
    let duration = 1000u64;
    env.ledger().set_timestamp(start_time);

    treasury_client.allocate_budget(&admin, &old_beneficiary, &amount, &start_time, &duration, &request_id(&env));

    env.ledger().set_timestamp(start_time + 500);

    let claimed = treasury_client.claim(&old_beneficiary);
    assert_eq!(claimed, 500);
    assert_eq!(token_client.balance(&old_beneficiary), 500);

    treasury_client.rotate_beneficiary(&admin, &old_beneficiary, &new_beneficiary);

    assert_eq!(
        treasury_client.try_get_unlocked(&old_beneficiary),
        Err(Ok(TreasuryError::StreamNotFound))
    );

    assert_eq!(treasury_client.get_unlocked(&new_beneficiary), 500);

    env.ledger().set_timestamp(start_time + 1000);
    assert_eq!(treasury_client.get_unlocked(&new_beneficiary), 500);

    let claimed_remaining = treasury_client.claim(&new_beneficiary);
    assert_eq!(claimed_remaining, 500);
    assert_eq!(token_client.balance(&new_beneficiary), 500);

    assert_eq!(token_client.balance(&old_beneficiary), 500);
    assert_eq!(token_client.balance(&new_beneficiary), 500);
}

#[test]
fn test_rotate_beneficiary_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let unauthorized = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let new_beneficiary = Address::generate(&env);
    let token_admin = Address::generate(&env);

    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_admin_client = token::StellarAssetClient::new(&env, &token_id.address());

    let treasury_id = env.register(TreasuryContract, ());
    let treasury_client = TreasuryContractClient::new(&env, &treasury_id);

    treasury_client.initialize(&admin, &token_id.address());

    let amount = 1000i128;
    token_admin_client.mint(&admin, &amount);

    let start_time = 1000u64;
    let duration = 1000u64;
    env.ledger().set_timestamp(start_time);

    treasury_client.allocate_budget(&admin, &beneficiary, &amount, &start_time, &duration, &request_id(&env));

    assert_eq!(
        treasury_client.try_rotate_beneficiary(&unauthorized, &beneficiary, &new_beneficiary),
        Err(Ok(TreasuryError::Unauthorized))
    );
}

#[test]
fn test_rotate_beneficiary_same_address() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let token_admin = Address::generate(&env);

    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_admin_client = token::StellarAssetClient::new(&env, &token_id.address());

    let treasury_id = env.register(TreasuryContract, ());
    let treasury_client = TreasuryContractClient::new(&env, &treasury_id);

    treasury_client.initialize(&admin, &token_id.address());

    let amount = 1000i128;
    token_admin_client.mint(&admin, &amount);

    let start_time = 1000u64;
    let duration = 1000u64;
    env.ledger().set_timestamp(start_time);

    treasury_client.allocate_budget(&admin, &beneficiary, &amount, &start_time, &duration, &request_id(&env));

    assert_eq!(
        treasury_client.try_rotate_beneficiary(&admin, &beneficiary, &beneficiary),
        Err(Ok(TreasuryError::SameBeneficiary))
    );
}

#[test]
fn test_rotate_beneficiary_stream_not_found() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let old_beneficiary = Address::generate(&env);
    let new_beneficiary = Address::generate(&env);
    let token_admin = Address::generate(&env);

    let token_id = env.register_stellar_asset_contract_v2(token_admin.clone());

    let treasury_id = env.register(TreasuryContract, ());
    let treasury_client = TreasuryContractClient::new(&env, &treasury_id);

    treasury_client.initialize(&admin, &token_id.address());

    assert_eq!(
        treasury_client.try_rotate_beneficiary(&admin, &old_beneficiary, &new_beneficiary),
        Err(Ok(TreasuryError::StreamNotFound))
    );
}
