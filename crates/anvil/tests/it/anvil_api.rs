//! tests for custom anvil endpoints
use crate::{
    abi::{BinanceUSD, Greeter, Multicall, SolGreeter},
    fork::fork_config,
    utils::ethers_http_provider,
};
use alloy_primitives::{Address as rAddress, B256, U256 as rU256};
use alloy_providers::provider::TempProvider;
use alloy_rpc_types::BlockNumberOrTag;
use alloy_sol_types::SolCall;
use anvil::{eth::api::CLIENT_VERSION, spawn, Hardfork, NodeConfig};
use anvil_core::{
    eth::EthRequest,
    types::{AnvilMetadata, ForkedNetwork, Forking, NodeEnvironment, NodeForkConfig, NodeInfo},
};
use ethers::{
    prelude::{Middleware, SignerMiddleware},
    types::{
        transaction::eip2718::TypedTransaction, Address, BlockNumber, Eip1559TransactionRequest,
        TransactionRequest, H256, U256, U64,
    },
    utils::hex,
};
use foundry_common::types::{ToAlloy, ToEthers};
use foundry_evm::revm::primitives::SpecId;
use std::{
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime},
};

#[tokio::test(flavor = "multi_thread")]
async fn can_set_gas_price() {
    let (api, handle) = spawn(NodeConfig::test().with_hardfork(Some(Hardfork::Berlin))).await;
    let provider = handle.http_provider();

    let gas_price = rU256::from(1337u64);
    api.anvil_set_min_gas_price(gas_price).await.unwrap();
    assert_eq!(gas_price, provider.get_gas_price().await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn can_set_block_gas_limit() {
    let (api, _) = spawn(NodeConfig::test().with_hardfork(Some(Hardfork::Berlin))).await;

    let block_gas_limit = rU256::from(1337u64);
    assert!(api.evm_set_block_gas_limit(block_gas_limit).unwrap());
    // Mine a new block, and check the new block gas limit
    api.mine_one().await;
    let latest_block =
        api.block_by_number(alloy_rpc_types::BlockNumberOrTag::Latest).await.unwrap().unwrap();
    assert_eq!(block_gas_limit, latest_block.header.gas_limit);
}

// Ref <https://github.com/foundry-rs/foundry/issues/2341>
#[tokio::test(flavor = "multi_thread")]
async fn can_set_storage() {
    let (api, _handle) = spawn(NodeConfig::test()).await;
    let s = r#"{"jsonrpc": "2.0", "method": "hardhat_setStorageAt", "id": 1, "params": ["0xe9e7CEA3DedcA5984780Bafc599bD69ADd087D56", "0xa6eef7e35abe7026729641147f7915573c7e97b47efa546f5f6e3230263bcb49", "0x0000000000000000000000000000000000000000000000000000000000003039"]}"#;
    let req = serde_json::from_str::<EthRequest>(s).unwrap();
    let (addr, slot, val) = match req.clone() {
        EthRequest::SetStorageAt(addr, slot, val) => (addr, slot, val),
        _ => unreachable!(),
    };

    api.execute(req).await;

    let storage_value = api.storage_at(addr, slot, None).await.unwrap();
    assert_eq!(val, storage_value);
    assert_eq!(val, B256::from(rU256::from(12345)));
}

#[tokio::test(flavor = "multi_thread")]
async fn can_impersonate_account() {
    let (api, handle) = spawn(NodeConfig::test()).await;
    let provider = ethers_http_provider(&handle.http_endpoint());

    let impersonate = Address::random();
    let to = Address::random();
    let val = 1337u64;
    let funding = U256::from(1e18 as u64);
    // fund the impersonated account
    api.anvil_set_balance(impersonate.to_alloy(), funding.to_alloy()).await.unwrap();

    let balance = api.balance(impersonate.to_alloy(), None).await.unwrap();
    assert_eq!(balance, funding.to_alloy());

    let tx = TransactionRequest::new().from(impersonate).to(to).value(val);

    let res = provider.send_transaction(tx.clone(), None).await;
    res.unwrap_err();

    api.anvil_impersonate_account(impersonate.to_alloy()).await.unwrap();
    assert!(api.accounts().unwrap().contains(&impersonate.to_alloy()));

    let res = provider.send_transaction(tx.clone(), None).await.unwrap().await.unwrap().unwrap();
    assert_eq!(res.from, impersonate);

    let nonce = provider.get_transaction_count(impersonate, None).await.unwrap();
    assert_eq!(nonce, 1u64.into());

    let balance = provider.get_balance(to, None).await.unwrap();
    assert_eq!(balance, val.into());

    api.anvil_stop_impersonating_account(impersonate.to_alloy()).await.unwrap();
    let res = provider.send_transaction(tx, None).await;
    res.unwrap_err();
}

#[tokio::test(flavor = "multi_thread")]
async fn can_auto_impersonate_account() {
    let (api, handle) = spawn(NodeConfig::test()).await;
    let provider = ethers_http_provider(&handle.http_endpoint());

    let impersonate = Address::random();
    let to = Address::random();
    let val = 1337u64;
    let funding = U256::from(1e18 as u64);
    // fund the impersonated account
    api.anvil_set_balance(impersonate.to_alloy(), funding.to_alloy()).await.unwrap();

    let balance = api.balance(impersonate.to_alloy(), None).await.unwrap();
    assert_eq!(balance, funding.to_alloy());

    let tx = TransactionRequest::new().from(impersonate).to(to).value(val);

    let res = provider.send_transaction(tx.clone(), None).await;
    res.unwrap_err();

    api.anvil_auto_impersonate_account(true).await.unwrap();

    let res = provider.send_transaction(tx.clone(), None).await.unwrap().await.unwrap().unwrap();
    assert_eq!(res.from, impersonate);

    let nonce = provider.get_transaction_count(impersonate, None).await.unwrap();
    assert_eq!(nonce, 1u64.into());

    let balance = provider.get_balance(to, None).await.unwrap();
    assert_eq!(balance, val.into());

    api.anvil_auto_impersonate_account(false).await.unwrap();
    let res = provider.send_transaction(tx, None).await;
    res.unwrap_err();

    // explicitly impersonated accounts get returned by `eth_accounts`
    api.anvil_impersonate_account(impersonate.to_alloy()).await.unwrap();
    assert!(api.accounts().unwrap().contains(&impersonate.to_alloy()));
}

#[tokio::test(flavor = "multi_thread")]
async fn can_impersonate_contract() {
    let (api, handle) = spawn(NodeConfig::test()).await;
    let provider = ethers_http_provider(&handle.http_endpoint());

    let wallet = handle.dev_wallets().next().unwrap().to_ethers();
    let provider = Arc::new(SignerMiddleware::new(provider, wallet));

    let greeter_contract =
        Greeter::deploy(provider, "Hello World!".to_string()).unwrap().send().await.unwrap();
    let greeter = SolGreeter::new(greeter_contract.address().to_alloy(), handle.http_provider());
    let impersonate = greeter_contract.address();

    let to = Address::random();
    let val = 1337u64;

    let provider = ethers_http_provider(&handle.http_endpoint());

    // fund the impersonated account
    api.anvil_set_balance(impersonate.to_alloy(), U256::from(1e18 as u64).to_alloy())
        .await
        .unwrap();

    let tx = TransactionRequest::new().from(impersonate).to(to).value(val);

    let res = provider.send_transaction(tx.clone(), None).await;
    res.unwrap_err();

    let greeting = greeter.greet().call().await.unwrap()._0;
    assert_eq!("Hello World!", greeting);

    api.anvil_impersonate_account(impersonate.to_alloy()).await.unwrap();

    let res = provider.send_transaction(tx.clone(), None).await.unwrap().await.unwrap().unwrap();
    assert_eq!(res.from, impersonate);

    let balance = provider.get_balance(to, None).await.unwrap();
    assert_eq!(balance, val.into());

    api.anvil_stop_impersonating_account(impersonate.to_alloy()).await.unwrap();
    let res = provider.send_transaction(tx, None).await;
    res.unwrap_err();

    let greeting = greeter.greet().call().await.unwrap()._0;
    assert_eq!("Hello World!", greeting);
}

#[tokio::test(flavor = "multi_thread")]
async fn can_impersonate_gnosis_safe() {
    let (api, handle) = spawn(fork_config()).await;
    let provider = handle.http_provider();

    // <https://help.safe.global/en/articles/40824-i-don-t-remember-my-safe-address-where-can-i-find-it>
    let safe: rAddress = "0xA063Cb7CFd8E57c30c788A0572CBbf2129ae56B6".parse().unwrap();

    let code = provider.get_code_at(safe, BlockNumberOrTag::Latest.into()).await.unwrap();
    assert!(!code.is_empty());

    api.anvil_impersonate_account(safe).await.unwrap();

    let code = provider.get_code_at(safe, BlockNumberOrTag::Latest.into()).await.unwrap();
    assert!(!code.is_empty());

    let balance = rU256::from(1e18 as u64);
    // fund the impersonated account
    api.anvil_set_balance(safe, balance).await.unwrap();

    let on_chain_balance = provider.get_balance(safe, None).await.unwrap();
    assert_eq!(on_chain_balance, balance);

    api.anvil_stop_impersonating_account(safe).await.unwrap();

    let code = provider.get_code_at(safe, BlockNumberOrTag::Latest.into()).await.unwrap();
    // code is added back after stop impersonating
    assert!(!code.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn can_impersonate_multiple_account() {
    let (api, handle) = spawn(NodeConfig::test()).await;
    let provider = ethers_http_provider(&handle.http_endpoint());

    let impersonate0 = Address::random();
    let impersonate1 = Address::random();
    let to = Address::random();

    let val = 1337u64;
    let funding = U256::from(1e18 as u64);
    // fund the impersonated accounts
    api.anvil_set_balance(impersonate0.to_alloy(), funding.to_alloy()).await.unwrap();
    api.anvil_set_balance(impersonate1.to_alloy(), funding.to_alloy()).await.unwrap();

    let tx = TransactionRequest::new().from(impersonate0).to(to).value(val);

    api.anvil_impersonate_account(impersonate0.to_alloy()).await.unwrap();
    api.anvil_impersonate_account(impersonate1.to_alloy()).await.unwrap();

    let res0 = provider.send_transaction(tx.clone(), None).await.unwrap().await.unwrap().unwrap();
    assert_eq!(res0.from, impersonate0);

    let nonce = provider.get_transaction_count(impersonate0, None).await.unwrap();
    assert_eq!(nonce, 1u64.into());

    let receipt = provider.get_transaction_receipt(res0.transaction_hash).await.unwrap().unwrap();
    assert_eq!(res0, receipt);

    let res1 = provider
        .send_transaction(tx.from(impersonate1), None)
        .await
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(res1.from, impersonate1);

    let nonce = provider.get_transaction_count(impersonate1, None).await.unwrap();
    assert_eq!(nonce, 1u64.into());

    let receipt = provider.get_transaction_receipt(res1.transaction_hash).await.unwrap().unwrap();
    assert_eq!(res1, receipt);

    assert_ne!(res0, res1);
}

#[tokio::test(flavor = "multi_thread")]
async fn can_mine_manually() {
    let (api, handle) = spawn(NodeConfig::test()).await;
    let provider = handle.http_provider();

    let start_num = provider.get_block_number().await.unwrap();

    for (idx, _) in std::iter::repeat(()).take(10).enumerate() {
        api.evm_mine(None).await.unwrap();
        let num = provider.get_block_number().await.unwrap();
        assert_eq!(num, start_num + (idx as u64) + 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_set_next_timestamp() {
    let (api, handle) = spawn(NodeConfig::test()).await;
    let provider = handle.http_provider();

    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();

    let next_timestamp = now + Duration::from_secs(60);

    // mock timestamp
    api.evm_set_next_block_timestamp(next_timestamp.as_secs()).unwrap();

    api.evm_mine(None).await.unwrap();

    let block = provider.get_block(BlockNumberOrTag::Latest.into(), false).await.unwrap().unwrap();

    assert_eq!(block.header.number.unwrap().to::<u64>(), 1);
    assert_eq!(block.header.timestamp.to::<u64>(), next_timestamp.as_secs());

    api.evm_mine(None).await.unwrap();

    let next = provider.get_block(BlockNumberOrTag::Latest.into(), false).await.unwrap().unwrap();
    assert_eq!(next.header.number.unwrap().to::<u64>(), 2);

    assert!(next.header.timestamp > block.header.timestamp);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_evm_set_time() {
    let (api, handle) = spawn(NodeConfig::test()).await;
    let provider = handle.http_provider();

    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();

    let timestamp = now + Duration::from_secs(120);

    // mock timestamp
    api.evm_set_time(timestamp.as_secs()).unwrap();

    // mine a block
    api.evm_mine(None).await.unwrap();
    let block = provider.get_block(BlockNumberOrTag::Latest.into(), false).await.unwrap().unwrap();

    assert!(block.header.timestamp.to::<u64>() >= timestamp.as_secs());

    api.evm_mine(None).await.unwrap();
    let next = provider.get_block(BlockNumberOrTag::Latest.into(), false).await.unwrap().unwrap();

    assert!(next.header.timestamp > block.header.timestamp);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_evm_set_time_in_past() {
    let (api, handle) = spawn(NodeConfig::test()).await;
    let provider = handle.http_provider();

    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();

    let timestamp = now - Duration::from_secs(120);

    // mock timestamp
    api.evm_set_time(timestamp.as_secs()).unwrap();

    // mine a block
    api.evm_mine(None).await.unwrap();
    let block = provider.get_block(BlockNumberOrTag::Latest.into(), false).await.unwrap().unwrap();

    assert!(block.header.timestamp.to::<u64>() >= timestamp.as_secs());
    assert!(block.header.timestamp.to::<u64>() < now.as_secs());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_timestamp_interval() {
    let (api, handle) = spawn(NodeConfig::test()).await;
    let provider = handle.http_provider();

    api.evm_mine(None).await.unwrap();
    let interval = 10;

    for _ in 0..5 {
        let block =
            provider.get_block(BlockNumberOrTag::Latest.into(), false).await.unwrap().unwrap();

        // mock timestamp
        api.evm_set_block_timestamp_interval(interval).unwrap();
        api.evm_mine(None).await.unwrap();

        let new_block =
            provider.get_block(BlockNumberOrTag::Latest.into(), false).await.unwrap().unwrap();

        assert_eq!(new_block.header.timestamp, block.header.timestamp + rU256::from(interval));
    }

    let block = provider.get_block(BlockNumberOrTag::Latest.into(), false).await.unwrap().unwrap();

    let next_timestamp = block.header.timestamp.to::<u64>() + 50;
    api.evm_set_next_block_timestamp(next_timestamp).unwrap();

    api.evm_mine(None).await.unwrap();
    let block = provider.get_block(BlockNumberOrTag::Latest.into(), false).await.unwrap().unwrap();
    assert_eq!(block.header.timestamp.to::<u64>(), next_timestamp);

    api.evm_mine(None).await.unwrap();

    let block = provider.get_block(BlockNumberOrTag::Latest.into(), false).await.unwrap().unwrap();
    // interval also works after setting the next timestamp manually
    assert_eq!(block.header.timestamp.to::<u64>(), next_timestamp + interval);

    assert!(api.evm_remove_block_timestamp_interval().unwrap());

    api.evm_mine(None).await.unwrap();
    let new_block =
        provider.get_block(BlockNumberOrTag::Latest.into(), false).await.unwrap().unwrap();

    // offset is applied correctly after resetting the interval
    assert!(new_block.header.timestamp > block.header.timestamp);

    api.evm_mine(None).await.unwrap();
    let another_block =
        provider.get_block(BlockNumberOrTag::Latest.into(), false).await.unwrap().unwrap();
    // check interval is disabled
    assert!(another_block.header.timestamp - new_block.header.timestamp < rU256::from(interval));
}

// <https://github.com/foundry-rs/foundry/issues/2341>
#[tokio::test(flavor = "multi_thread")]
async fn test_can_set_storage_bsc_fork() {
    let (api, handle) =
        spawn(NodeConfig::test().with_eth_rpc_url(Some("https://bsc-dataseed.binance.org/"))).await;

    let busd_addr: Address = "0xe9e7CEA3DedcA5984780Bafc599bD69ADd087D56".parse().unwrap();
    let idx: U256 =
        "0xa6eef7e35abe7026729641147f7915573c7e97b47efa546f5f6e3230263bcb49".parse().unwrap();
    let value: H256 =
        "0x0000000000000000000000000000000000000000000000000000000000003039".parse().unwrap();

    api.anvil_set_storage_at(busd_addr.to_alloy(), idx.to_alloy(), value.to_alloy()).await.unwrap();
    let storage = api.storage_at(busd_addr.to_alloy(), idx.to_alloy(), None).await.unwrap();
    assert_eq!(storage.to_ethers(), value);

    let input =
        hex::decode("70a082310000000000000000000000000000000000000000000000000000000000000000")
            .unwrap();

    let provider = handle.http_provider();

    let contract = BinanceUSD::new(busd_addr.to_alloy(), provider);
    let busd_call_input = BinanceUSD::balanceOfCall::abi_decode(&input, false).unwrap();

    let balance = contract.balanceOf(busd_call_input.account).call().await.unwrap()._0;

    assert_eq!(balance, rU256::from(12345u64));
}

#[tokio::test(flavor = "multi_thread")]
async fn can_get_node_info() {
    let (api, handle) = spawn(NodeConfig::test()).await;

    let node_info = api.anvil_node_info().await.unwrap();

    let provider = handle.http_provider();

    let block_number = provider.get_block_number().await.unwrap();
    let block = provider.get_block(block_number.into(), false).await.unwrap().unwrap();

    let expected_node_info = NodeInfo {
        current_block_number: U64([0]).to_alloy(),
        current_block_timestamp: 1,
        current_block_hash: block.header.hash.unwrap(),
        hard_fork: SpecId::SHANGHAI,
        transaction_order: "fees".to_owned(),
        environment: NodeEnvironment {
            base_fee: U256::from_str("0x3b9aca00").unwrap().to_alloy(),
            chain_id: 0x7a69,
            gas_limit: U256::from_str("0x1c9c380").unwrap().to_alloy(),
            gas_price: U256::from_str("0x77359400").unwrap().to_alloy(),
        },
        fork_config: NodeForkConfig {
            fork_url: None,
            fork_block_number: None,
            fork_retry_backoff: None,
        },
    };

    assert_eq!(node_info, expected_node_info);
}

#[tokio::test(flavor = "multi_thread")]
async fn can_get_metadata() {
    let (api, handle) = spawn(NodeConfig::test()).await;

    let metadata = api.anvil_metadata().await.unwrap();

    let provider = handle.http_provider();

    let block_number = provider.get_block_number().await.unwrap();
    let chain_id = provider.get_chain_id().await.unwrap().to::<u64>();
    let block = provider.get_block(block_number.into(), false).await.unwrap().unwrap();

    let expected_metadata = AnvilMetadata {
        latest_block_hash: block.header.hash.unwrap(),
        latest_block_number: block_number,
        chain_id,
        client_version: CLIENT_VERSION,
        instance_id: api.instance_id(),
        forked_network: None,
        snapshots: Default::default(),
    };

    assert_eq!(metadata, expected_metadata);
}

#[tokio::test(flavor = "multi_thread")]
async fn can_get_metadata_on_fork() {
    let (api, handle) =
        spawn(NodeConfig::test().with_eth_rpc_url(Some("https://bsc-dataseed.binance.org/"))).await;
    let provider = handle.http_provider();

    let metadata = api.anvil_metadata().await.unwrap();

    let block_number = provider.get_block_number().await.unwrap();
    let chain_id = provider.get_chain_id().await.unwrap().to::<u64>();
    let block = provider.get_block(block_number.into(), false).await.unwrap().unwrap();

    let expected_metadata = AnvilMetadata {
        latest_block_hash: block.header.hash.unwrap(),
        latest_block_number: block_number,
        chain_id,
        client_version: CLIENT_VERSION,
        instance_id: api.instance_id(),
        forked_network: Some(ForkedNetwork {
            chain_id,
            fork_block_number: block_number,
            fork_block_hash: block.header.hash.unwrap(),
        }),
        snapshots: Default::default(),
    };

    assert_eq!(metadata, expected_metadata);
}

#[tokio::test(flavor = "multi_thread")]
async fn metadata_changes_on_reset() {
    let (api, _) =
        spawn(NodeConfig::test().with_eth_rpc_url(Some("https://bsc-dataseed.binance.org/"))).await;

    let metadata = api.anvil_metadata().await.unwrap();
    let instance_id = metadata.instance_id;

    api.anvil_reset(Some(Forking { json_rpc_url: None, block_number: None })).await.unwrap();

    let new_metadata = api.anvil_metadata().await.unwrap();
    let new_instance_id = new_metadata.instance_id;

    assert_ne!(instance_id, new_instance_id);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_get_transaction_receipt() {
    let (api, handle) = spawn(NodeConfig::test()).await;
    let provider = ethers_http_provider(&handle.http_endpoint());

    // set the base fee
    let new_base_fee = U256::from(1_000);
    api.anvil_set_next_block_base_fee_per_gas(new_base_fee.to_alloy()).await.unwrap();

    // send a EIP-1559 transaction
    let tx =
        TypedTransaction::Eip1559(Eip1559TransactionRequest::new().gas(U256::from(30_000_000)));
    let receipt =
        provider.send_transaction(tx.clone(), None).await.unwrap().await.unwrap().unwrap();

    // the block should have the new base fee
    let block = provider.get_block(BlockNumber::Latest).await.unwrap().unwrap();
    assert_eq!(block.base_fee_per_gas.unwrap().as_u64(), new_base_fee.as_u64());

    // mine block
    api.evm_mine(None).await.unwrap();

    // the transaction receipt should have the original effective gas price
    let new_receipt = provider.get_transaction_receipt(receipt.transaction_hash).await.unwrap();
    assert_eq!(
        receipt.effective_gas_price.unwrap().as_u64(),
        new_receipt.unwrap().effective_gas_price.unwrap().as_u64()
    );
}

// test can set chain id
#[tokio::test(flavor = "multi_thread")]
async fn test_set_chain_id() {
    let (api, handle) = spawn(NodeConfig::test()).await;
    let provider = handle.http_provider();
    let chain_id = provider.get_chain_id().await.unwrap();
    assert_eq!(chain_id.to::<u64>(), 31337);

    let chain_id = 1234;
    api.anvil_set_chain_id(chain_id).await.unwrap();

    let chain_id = provider.get_chain_id().await.unwrap();
    assert_eq!(chain_id.to::<u64>(), 1234);
}

// <https://github.com/foundry-rs/foundry/issues/6096>
#[tokio::test(flavor = "multi_thread")]
async fn test_fork_revert_next_block_timestamp() {
    let (api, _handle) = spawn(fork_config()).await;

    // Mine a new block, and check the new block gas limit
    api.mine_one().await;
    let latest_block = api.block_by_number(BlockNumberOrTag::Latest).await.unwrap().unwrap();

    let snapshot_id = api.evm_snapshot().await.unwrap();
    api.mine_one().await;
    api.evm_revert(snapshot_id).await.unwrap();
    let block = api.block_by_number(BlockNumberOrTag::Latest).await.unwrap().unwrap();
    assert_eq!(block, latest_block);

    api.mine_one().await;
    let block = api.block_by_number(BlockNumberOrTag::Latest).await.unwrap().unwrap();
    assert!(block.header.timestamp > latest_block.header.timestamp);
}

// test that after a snapshot revert, the env block is reset
// to its correct value (block number, etc.)
#[tokio::test(flavor = "multi_thread")]
async fn test_fork_revert_call_latest_block_timestamp() {
    let (api, handle) = spawn(fork_config()).await;

    // Mine a new block, and check the new block gas limit
    api.mine_one().await;
    let latest_block = api.block_by_number(BlockNumberOrTag::Latest).await.unwrap().unwrap();

    let snapshot_id = api.evm_snapshot().await.unwrap();
    api.mine_one().await;
    api.evm_revert(snapshot_id).await.unwrap();

    let multicall = Multicall::new(
        rAddress::from_str("0xeefba1e63905ef1d7acba5a8513c70307c1ce441").unwrap(),
        handle.http_provider(),
    );

    assert_eq!(
        multicall.getCurrentBlockTimestamp().call().await.unwrap().timestamp,
        latest_block.header.timestamp
    );
    assert_eq!(
        multicall.getCurrentBlockDifficulty().call().await.unwrap().difficulty,
        latest_block.header.difficulty
    );
    assert_eq!(
        multicall.getCurrentBlockGasLimit().call().await.unwrap().gaslimit,
        latest_block.header.gas_limit
    );
    assert_eq!(
        multicall.getCurrentBlockCoinbase().call().await.unwrap().coinbase,
        latest_block.header.miner
    );
}
