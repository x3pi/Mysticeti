package main

import (
	"bufio"
	"context"
	"crypto/ecdsa"
	"crypto/tls"
	"encoding/json"
	"fmt"
	"log"
	"math/big"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"

	"github.com/ethereum/go-ethereum/accounts/abi"
	"github.com/ethereum/go-ethereum/accounts/abi/bind"
	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/ethclient"
	"github.com/ethereum/go-ethereum/rpc"
	"github.com/gorilla/websocket"
	"github.com/joho/godotenv"
)

// Config holds deployment configuration
type Config struct {
	RPCUrl             string
	DeployerPrivateKey string
	SourceNationID     *big.Int
	DestNationID       *big.Int
}

// BytecodeData holds bytecode from JSON file
type BytecodeData struct {
	CrossChain string `json:"cross_chain"`
}

var (
	globalClient          *ethclient.Client
	globalAuth            *bind.TransactOpts
	globalContractAddress common.Address
	globalParsedABI       abi.ABI
)

func main() {
	// Load environment variables
	err := godotenv.Load(".env.crosschain")
	if err != nil {
		log.Printf("Warning: Error loading .env.crosschain file, trying .env: %v", err)
		godotenv.Load(".env")
	}

	// Parse sourceNationId and destNationId from env
	sourceNationID := getEnvBigInt("SOURCE_NATION_ID", "1")
	destNationID := getEnvBigInt("DEST_NATION_ID", "2")

	// Initialize deployment configuration
	config := &Config{
		RPCUrl:             getEnv("RPC_URL", "http://192.168.1.234:8545"),
		DeployerPrivateKey: getEnv("PRIVATE_KEY", ""),
		SourceNationID:     sourceNationID,
		DestNationID:       destNationID,
	}

	if config.DeployerPrivateKey == "" {
		log.Fatal("PRIVATE_KEY is required in .env file")
	}

	var rpcClient *rpc.Client
	ctx := context.Background()
	rpcUrl := config.RPCUrl
	parsedURL, err := url.Parse(rpcUrl)
	if err != nil {
		log.Fatalf("Failed to parse RPC URL: %v", err)
	}

	switch parsedURL.Scheme {
	case "https":
		insecureTLSConfig := &tls.Config{InsecureSkipVerify: true}
		transport := &http.Transport{TLSClientConfig: insecureTLSConfig}
		httpClient := &http.Client{Transport: transport}
		rpcClient, err = rpc.DialHTTPWithClient(rpcUrl, httpClient)
	case "wss":
		dialer := *websocket.DefaultDialer
		dialer.TLSClientConfig = &tls.Config{InsecureSkipVerify: true}
		rpcClient, err = rpc.DialWebsocketWithDialer(ctx, rpcUrl, "", dialer)
	default:
		log.Printf("Connecting to RPC URL with default settings (scheme: %s).", parsedURL.Scheme)
		rpcClient, err = rpc.DialContext(ctx, rpcUrl)
	}

	if err != nil {
		log.Fatalf("Failed to connect to RPC: %v", err)
	}

	client := ethclient.NewClient(rpcClient)
	globalClient = client

	// Get deployer account
	deployerAuth, err := getDeployerAuth(client, config.DeployerPrivateKey)
	if err != nil {
		log.Fatalf("Failed to get deployer auth: %v", err)
	}
	globalAuth = deployerAuth

	log.Printf("🚀 Starting Cross-Chain Gateway Deployment...")
	log.Printf("📍 Deployer Address: %s", deployerAuth.From.Hex())
	log.Printf("🌐 RPC URL: %s", config.RPCUrl)
	log.Printf("🔗 Source Nation ID: %s", config.SourceNationID.String())
	log.Printf("🔗 Dest Nation ID: %s", config.DestNationID.String())
	log.Println("=====================================")

	// Step 1: Deploy CrossChainGateway contract
	log.Println("\n[1/2] Deploying CrossChainGateway contract...")
	contractAddress, err := deployCrossChainGateway(client, deployerAuth, config.SourceNationID, config.DestNationID)
	if err != nil {
		log.Fatalf("Failed to deploy CrossChainGateway: %v", err)
	}
	globalContractAddress = contractAddress
	log.Printf("✅ CrossChainGateway deployed at: %s", contractAddress.Hex())

	// Step 2: Verify deployment by checking nation IDs
	log.Println("\n[2/2] Verifying deployment...")
	if err := verifyDeployment(client, contractAddress); err != nil {
		log.Printf("⚠️  Warning: Verification failed: %v", err)
	} else {
		log.Println("✅ Deployment verified successfully")
	}

	log.Println("\n=====================================")
	log.Println("🎉 Cross-Chain Gateway Deployment Completed!")
	log.Println("=====================================")

	// Save deployment info to file
	deploymentInfo := map[string]interface{}{
		"crossChainGateway": contractAddress.Hex(),
		"sourceNationID":    config.SourceNationID.String(),
		"destNationID":      config.DestNationID.String(),
		"deployer":          deployerAuth.From.Hex(),
		"rpcUrl":            config.RPCUrl,
		"timestamp":         time.Now().Format(time.RFC3339),
	}

	saveDeploymentInfo(deploymentInfo)

	// Interactive menu
	showInteractiveMenu(client, deployerAuth, contractAddress)
}

// showInteractiveMenu displays interactive menu for contract interactions
func showInteractiveMenu(client *ethclient.Client, auth *bind.TransactOpts, contractAddress common.Address) {
	// Load ABI
	abiData, err := os.ReadFile("crossChainAbi.json")
	if err != nil {
		log.Printf("Error loading ABI for menu: %v", err)
		return
	}

	parsedABI, err := abi.JSON(strings.NewReader(string(abiData)))
	if err != nil {
		log.Printf("Error parsing ABI for menu: %v", err)
		return
	}
	globalParsedABI = parsedABI

	scanner := bufio.NewScanner(os.Stdin)

	for {
		fmt.Println("\n=====================================")
		fmt.Println("📋 Cross-Chain Gateway Menu")
		fmt.Println("=====================================")
		fmt.Println("1. Lock and Bridge (1 ETH to 0xbF2b4B9b9dFB6d23F7F0FC46981c2eC89f94A9F2)")
		fmt.Println("2. Check Balance")
		fmt.Println("3. Get Config")
		fmt.Println("0. Exit")
		fmt.Print("\nEnter your choice: ")

		if !scanner.Scan() {
			break
		}

		choice := strings.TrimSpace(scanner.Text())

		switch choice {
		case "1":
			callLockAndBridge(client, auth, contractAddress, parsedABI)
		case "2":
			checkBalance(client, auth.From)
		case "3":
			getConfig(client, contractAddress, parsedABI)
		case "0":
			fmt.Println("\n👋 Goodbye!")
			return
		default:
			fmt.Println("❌ Invalid choice. Please try again.")
		}
	}
}

// callLockAndBridge calls the lockAndBridge function with 1 ETH
func callLockAndBridge(client *ethclient.Client, auth *bind.TransactOpts, contractAddress common.Address, parsedABI abi.ABI) {
	fmt.Println("\n🔒 Calling lockAndBridge...")

	recipient := common.HexToAddress("0xbF2b4B9b9dFB6d23F7F0FC46981c2eC89f94A9F2")
	value := new(big.Int).Mul(big.NewInt(1), big.NewInt(1e18)) // 1 ETH

	fmt.Printf("   Recipient: %s\n", recipient.Hex())
	fmt.Printf("   Value: 1 ETH\n")

	// Get current nonce
	nonce, err := client.PendingNonceAt(context.Background(), auth.From)
	if err != nil {
		log.Printf("❌ Failed to get nonce: %v", err)
		return
	}

	// Prepare transaction auth
	txAuth := &bind.TransactOpts{
		From:     auth.From,
		Nonce:    big.NewInt(int64(nonce)),
		Signer:   auth.Signer,
		Value:    value,
		GasLimit: 500000,
		GasPrice: nil, // Use suggested gas price
	}

	// Create contract instance
	contract := bind.NewBoundContract(contractAddress, parsedABI, client, client, client)

	// Call lockAndBridge
	tx, err := contract.Transact(txAuth, "lockAndBridge", recipient)
	if err != nil {
		log.Printf("❌ Failed to send transaction: %v", err)
		return
	}

	fmt.Printf("📤 Transaction sent: %s\n", tx.Hash().Hex())
	fmt.Println("⏳ Waiting for confirmation...")

	// Wait for transaction receipt
	receipt, err := waitForTransaction(client, tx.Hash(), "lockAndBridge")
	if err != nil {
		log.Printf("❌ Transaction failed: %v", err)
		return
	}

	fmt.Printf("✅ Transaction confirmed!\n")
	fmt.Printf("   Block Number: %d\n", receipt.BlockNumber.Uint64())
	fmt.Printf("   Gas Used: %d\n", receipt.GasUsed)
	fmt.Printf("   Status: %d (1=success)\n", receipt.Status)

	// Try to decode events
	if len(receipt.Logs) > 0 {
		fmt.Printf("\n📜 Events emitted: %d\n", len(receipt.Logs))
		for i, vlog := range receipt.Logs {
			fmt.Printf("   Event %d: %d topics\n", i+1, len(vlog.Topics))
			if len(vlog.Topics) > 0 {
				fmt.Printf("      Topic 0: %s\n", vlog.Topics[0].Hex())
			}
		}
	}
}

// checkBalance checks the balance of the deployer
func checkBalance(client *ethclient.Client, address common.Address) {
	fmt.Println("\n💰 Checking Balance...")

	balance, err := client.BalanceAt(context.Background(), address, nil)
	if err != nil {
		log.Printf("❌ Failed to get balance: %v", err)
		return
	}

	ethBalance := new(big.Float).Quo(
		new(big.Float).SetInt(balance),
		big.NewFloat(1e18),
	)

	fmt.Printf("   Address: %s\n", address.Hex())
	fmt.Printf("   Balance: %s ETH\n", ethBalance.String())
}

// getConfig gets the contract configuration
func getConfig(client *ethclient.Client, contractAddress common.Address, parsedABI abi.ABI) {
	fmt.Println("\n⚙️  Getting Contract Config...")

	contract := bind.NewBoundContract(contractAddress, parsedABI, client, client, client)

	var result []interface{}
	callOpts := &bind.CallOpts{
		Pending: false,
		Context: context.Background(),
	}

	err := contract.Call(callOpts, &result, "getConfig")
	if err != nil {
		log.Printf("❌ Failed to call getConfig: %v", err)
		return
	}

	if len(result) >= 2 {
		sourceID := result[0].(*big.Int)
		destID := result[1].(*big.Int)
		fmt.Printf("   Source Nation ID: %s\n", sourceID.String())
		fmt.Printf("   Dest Nation ID: %s\n", destID.String())
	}
}

// deployCrossChainGateway deploys the CrossChainGateway contract
func deployCrossChainGateway(client *ethclient.Client, auth *bind.TransactOpts, sourceNationID, destNationID *big.Int) (common.Address, error) {
	log.Println("Deploying CrossChainGateway contract...")

	// Load ABI from crossChainAbi.json
	abiData, err := os.ReadFile("crossChainAbi.json")
	if err != nil {
		return common.Address{}, fmt.Errorf("failed to read ABI file: %w", err)
	}

	parsedABI, err := abi.JSON(strings.NewReader(string(abiData)))
	if err != nil {
		return common.Address{}, fmt.Errorf("failed to parse ABI: %w", err)
	}

	// Load bytecode from byteCode/byteCode.json
	bytecodeData, err := os.ReadFile("byteCode/byteCode.json")
	if err != nil {
		return common.Address{}, fmt.Errorf("failed to read bytecode file: %w", err)
	}

	var bytecodeFile BytecodeData
	if err := json.Unmarshal(bytecodeData, &bytecodeFile); err != nil {
		return common.Address{}, fmt.Errorf("failed to parse bytecode file: %w", err)
	}

	// Parse bytecode (remove 0x prefix if present)
	bytecodeHex := strings.TrimPrefix(bytecodeFile.CrossChain, "0x")
	bytecode := common.FromHex(bytecodeHex)

	if len(bytecode) == 0 {
		return common.Address{}, fmt.Errorf("contract bytecode not available")
	}

	// Pack constructor arguments (sourceNationID, destNationID)
	constructorArgs, err := parsedABI.Pack("", sourceNationID, destNationID)
	if err != nil {
		return common.Address{}, fmt.Errorf("failed to pack constructor arguments: %w", err)
	}

	// Combine bytecode with constructor arguments
	fullBytecode := append(bytecode, constructorArgs...)

	// Deploy the contract
	tx := types.NewContractCreation(
		auth.Nonce.Uint64(),
		auth.Value,
		auth.GasLimit,
		auth.GasPrice,
		fullBytecode,
	)

	signedTx, err := auth.Signer(auth.From, tx)
	if err != nil {
		return common.Address{}, fmt.Errorf("failed to sign transaction: %w", err)
	}

	err = client.SendTransaction(context.Background(), signedTx)
	if err != nil {
		return common.Address{}, fmt.Errorf("failed to send transaction: %w", err)
	}

	// Wait for transaction receipt
	receipt, err := waitForTransaction(client, signedTx.Hash(), "CrossChainGateway")
	if err != nil {
		return common.Address{}, fmt.Errorf("transaction failed: %w", err)
	}

	log.Printf("✅ Contract deployed at: %s", receipt.ContractAddress.Hex())
	log.Printf("⛽ Gas used: %d", receipt.GasUsed)

	return receipt.ContractAddress, nil
}

// verifyDeployment verifies the deployed contract
func verifyDeployment(client *ethclient.Client, contractAddress common.Address) error {
	// Load ABI
	abiData, err := os.ReadFile("crossChainAbi.json")
	if err != nil {
		return fmt.Errorf("failed to read ABI file: %w", err)
	}

	parsedABI, err := abi.JSON(strings.NewReader(string(abiData)))
	if err != nil {
		return fmt.Errorf("failed to parse ABI: %w", err)
	}

	// Create contract instance
	contract := bind.NewBoundContract(contractAddress, parsedABI, client, client, client)

	// Call getConfig function to verify
	var result []interface{}
	callOpts := &bind.CallOpts{
		Pending: false,
		Context: context.Background(),
	}

	err = contract.Call(callOpts, &result, "getConfig")
	if err != nil {
		return fmt.Errorf("failed to call getConfig: %w", err)
	}

	if len(result) >= 2 {
		sourceID := result[0].(*big.Int)
		destID := result[1].(*big.Int)
		log.Printf("✅ Contract Config - Source Nation ID: %s, Dest Nation ID: %s", sourceID.String(), destID.String())
	}

	return nil
}

// getDeployerAuth creates a transaction auth from private key
func getDeployerAuth(client *ethclient.Client, privateKeyHex string) (*bind.TransactOpts, error) {
	// Remove 0x prefix if present
	privateKeyHex = strings.TrimPrefix(privateKeyHex, "0x")

	privateKey, err := crypto.HexToECDSA(privateKeyHex)
	if err != nil {
		return nil, fmt.Errorf("invalid private key: %w", err)
	}

	publicKey := privateKey.Public()
	publicKeyECDSA, ok := publicKey.(*ecdsa.PublicKey)
	if !ok {
		return nil, fmt.Errorf("error casting public key to ECDSA")
	}

	fromAddress := crypto.PubkeyToAddress(*publicKeyECDSA)

	nonce, err := client.PendingNonceAt(context.Background(), fromAddress)
	if err != nil {
		return nil, fmt.Errorf("failed to get nonce: %w", err)
	}

	chainID, err := client.ChainID(context.Background())
	if err != nil {
		return nil, fmt.Errorf("failed to get chain ID: %w", err)
	}

	auth, err := bind.NewKeyedTransactorWithChainID(privateKey, chainID)
	if err != nil {
		return nil, fmt.Errorf("failed to create transactor: %w", err)
	}

	auth.Nonce = big.NewInt(int64(nonce))
	auth.Value = big.NewInt(0)
	auth.GasLimit = uint64(30000000)
	auth.GasPrice = nil // Use gas price suggestion

	return auth, nil
}

// waitForTransaction waits for a transaction to be mined
func waitForTransaction(client *ethclient.Client, txHash common.Hash, contractName string) (*types.Receipt, error) {
	log.Printf("⏳ Waiting for %s transaction to be mined...", contractName)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()

	for {
		receipt, err := client.TransactionReceipt(ctx, txHash)
		if err == nil {
			if receipt.Status == 1 {
				return receipt, nil
			}
			return nil, fmt.Errorf("transaction failed with status %d", receipt.Status)
		}

		select {
		case <-ctx.Done():
			return nil, fmt.Errorf("timeout waiting for transaction")
		case <-time.After(2 * time.Second):
			// Continue waiting
		}
	}
}

// getEnv gets environment variable with default value
func getEnv(key, defaultValue string) string {
	value := os.Getenv(key)
	if value == "" {
		return defaultValue
	}
	return value
}

// getEnvBigInt gets environment variable as big.Int with default value
func getEnvBigInt(key, defaultValue string) *big.Int {
	value := getEnv(key, defaultValue)
	result := new(big.Int)
	result.SetString(value, 10)
	return result
}

// saveDeploymentInfo saves deployment information to a file
func saveDeploymentInfo(info map[string]interface{}) {
	data, err := json.MarshalIndent(info, "", "  ")
	if err != nil {
		log.Printf("Warning: Failed to marshal deployment info: %v", err)
		return
	}

	filename := fmt.Sprintf("deployment_crosschain_%s.json", time.Now().Format("20060102_150405"))
	if err := os.WriteFile(filename, data, 0644); err != nil {
		log.Printf("Warning: Failed to save deployment info: %v", err)
		return
	}

	log.Printf("\n💾 Deployment info saved to: %s", filename)
}
