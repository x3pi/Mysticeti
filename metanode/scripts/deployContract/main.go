package main

// import (
// 	"context"
// 	"crypto/ecdsa"
// 	"crypto/tls"
// 	"encoding/json"
// 	"fmt"
// 	"log"
// 	"math/big"
// 	"net/http"
// 	"net/url"
// 	"os"
// 	"strings"
// 	"time"

// 	"github.com/ethereum/go-ethereum/accounts/abi"
// 	"github.com/ethereum/go-ethereum/accounts/abi/bind"
// 	"github.com/ethereum/go-ethereum/common"
// 	"github.com/ethereum/go-ethereum/core/types"
// 	"github.com/ethereum/go-ethereum/crypto"
// 	"github.com/ethereum/go-ethereum/ethclient"
// 	"github.com/ethereum/go-ethereum/rpc"
// 	"github.com/gorilla/websocket"
// 	"github.com/joho/godotenv"
// )

// // Config holds deployment configuration
// type Config struct {
// 	RPCUrl                 string
// 	DeployerPrivateKey     string
// 	RustStorageIPs         []string
// 	StorageServerAddresses []string
// }

// // DeployedContracts holds all deployed contract addresses
// type DeployedContracts struct {
// 	FilesContract common.Address
// }

// // ContractData holds ABI and bytecode
// type ContractData struct {
// 	ABI      string `json:"abi"`
// 	Bytecode string `json:"bytecode"`
// }

// func main() {
// 	// Load environment variables
// 	err := godotenv.Load(".env")
// 	if err != nil {
// 		log.Printf("Warning: Error loading .env file: %v", err)
// 	}

// 	// Initialize deployment configuration
// 	config := &Config{
// 		RPCUrl:                 getEnv("RPC_URL", "http://192.168.1.234:8545"),
// 		DeployerPrivateKey:     getEnv("PRIVATE_KEY", ""),
// 		RustStorageIPs:         parseCommaSeparatedString(getEnv("IP_RUST_STORAGE", "")),
// 		StorageServerAddresses: parseCommaSeparatedString(getEnv("STORAGE_SERVER_ADDRESS", "")),
// 	}

// 	if config.DeployerPrivateKey == "" {
// 		log.Fatal("PRIVATE_KEY is required in .env file")
// 	}

// 	var rpcClient *rpc.Client
// 	ctx := context.Background()
// 	rpcUrl := config.RPCUrl
// 	parsedURL, err := url.Parse(rpcUrl)
// 	if err != nil {
// 		log.Fatalf("Failed to parse RPC URL: %v", err)
// 	}

// 	switch parsedURL.Scheme {
// 	case "https":
// 		insecureTLSConfig := &tls.Config{InsecureSkipVerify: true}
// 		transport := &http.Transport{TLSClientConfig: insecureTLSConfig}
// 		httpClient := &http.Client{Transport: transport}
// 		rpcClient, err = rpc.DialHTTPWithClient(rpcUrl, httpClient)
// 	case "wss":
// 		dialer := *websocket.DefaultDialer
// 		dialer.TLSClientConfig = &tls.Config{InsecureSkipVerify: true}
// 		rpcClient, err = rpc.DialWebsocketWithDialer(ctx, rpcUrl, "", dialer)
// 	default:
// 		log.Printf("Connecting to RPC URL with default settings (scheme: %s).", parsedURL.Scheme)
// 		rpcClient, err = rpc.DialContext(ctx, rpcUrl)
// 	}

// 	if err != nil {
// 		log.Fatalf("Failed to connect to RPC: %v", err)
// 	}

// 	client := ethclient.NewClient(rpcClient)

// 	// Get deployer account
// 	deployerAuth, err := getDeployerAuth(client, config.DeployerPrivateKey)
// 	if err != nil {
// 		log.Fatalf("Failed to get deployer auth: %v", err)
// 	}

// 	log.Printf("🚀 Starting deployment process...")
// 	log.Printf("📍 Deployer Address: %s", deployerAuth.From.Hex())
// 	log.Printf("🌐 RPC URL: %s", config.RPCUrl)
// 	log.Println("=====================================")

// 	// Step 1: Deploy Files Implementation contract
// 	log.Println("\n[1/5] Deploying Files Implementation contract...")
// 	implementationAddress, err := deployFilesImplementation(client, deployerAuth)
// 	if err != nil {
// 		log.Fatalf("Failed to deploy Files implementation: %v", err)
// 	}
// 	log.Printf("✅ Files Implementation deployed at: %s", implementationAddress.Hex())

// 	// Step 2: Deploy FileProxy contract
// 	log.Println("\n[2/5] Deploying FileProxy contract...")
// 	proxyAddress, err := deployFileProxy(client, deployerAuth, implementationAddress)
// 	if err != nil {
// 		log.Fatalf("Failed to deploy FileProxy: %v", err)
// 	}
// 	log.Printf("✅ FileProxy deployed at: %s", proxyAddress.Hex())

// 	// Step 3: Create contract instance using proxy address
// 	abiData, err := os.ReadFile("fileAbi.json")
// 	if err != nil {
// 		log.Fatalf("Failed to read ABI file: %v", err)
// 	}
// 	parsedABI, err := abi.JSON(strings.NewReader(string(abiData)))
// 	if err != nil {
// 		log.Fatalf("Failed to parse ABI: %v", err)
// 	}
// 	filesContract := bind.NewBoundContract(proxyAddress, parsedABI, client, client, client)

// 	// Step 4: Initialize the contract through proxy
// 	log.Println("\n[3/5] Initializing contract through proxy...")
// 	if err := initializeContract(client, deployerAuth, filesContract); err != nil {
// 		log.Fatalf("Failed to initialize contract: %v", err)
// 	}
// 	log.Println("✅ Contract initialized successfully")

// 	// Step 5: Set Rust Server Addresses if configured
// 	if len(config.RustStorageIPs) > 0 {
// 		log.Println("\n[4/5] Setting Rust Server Addresses...")
// 		if err := setRustServerAddresses(client, deployerAuth, filesContract, config.RustStorageIPs); err != nil {
// 			log.Printf("❌ Failed to set Rust server addresses: %v", err)
// 		} else {
// 			log.Printf("✅ Successfully set %d Rust server addresses", len(config.RustStorageIPs))
// 		}
// 	}

// 	// Step 6: Add Storage Servers if configured
// 	if len(config.StorageServerAddresses) > 0 {
// 		log.Println("\n[5/5] Adding Storage Servers...")
// 		for i, storageAddr := range config.StorageServerAddresses {
// 			if !common.IsHexAddress(storageAddr) {
// 				log.Printf("⚠️  Skipping invalid storage server address: %s", storageAddr)
// 				continue
// 			}
// 			storageServer := common.HexToAddress(storageAddr)
// 			if err := addStorageServer(client, deployerAuth, filesContract, storageServer); err != nil {
// 				log.Printf("❌ Failed to add storage server %d (%s): %v", i+1, storageAddr, err)
// 			} else {
// 				log.Printf("✅ Successfully added storage server %d: %s", i+1, storageAddr)
// 			}
// 		}
// 	}

// 	log.Println("\n=====================================")
// 	log.Println("🎉 All deployment and configuration completed!")
// 	log.Println("=====================================")

// 	// Save deployment info to file
// 	deploymentInfo := map[string]interface{}{
// 		"implementationContract": implementationAddress.Hex(),
// 		"proxyContract":          proxyAddress.Hex(),
// 		"deployer":               deployerAuth.From.Hex(),
// 		"rpcUrl":                 config.RPCUrl,
// 		"rustServerIPs":          config.RustStorageIPs,
// 		"storageServerAddresses": config.StorageServerAddresses,
// 		"timestamp":              time.Now().Format(time.RFC3339),
// 	}

// 	saveDeploymentInfo(deploymentInfo)
// }

// // deployFilesImplementation deploys the Files implementation contract
// func deployFilesImplementation(client *ethclient.Client, auth *bind.TransactOpts) (common.Address, error) {
// 	log.Println("Deploying Files implementation contract...")

// 	// Load ABI from fileAbi.json
// 	abiData, err := os.ReadFile("fileAbi.json")
// 	if err != nil {
// 		return common.Address{}, fmt.Errorf("failed to read ABI file: %w", err)
// 	}

// 	parsedABI, err := abi.JSON(strings.NewReader(string(abiData)))
// 	if err != nil {
// 		return common.Address{}, fmt.Errorf("failed to parse ABI: %w", err)
// 	}

// 	// Load bytecode from byteCode/byteCode.json
// 	type BytecodeFile struct {
// 		File      string `json:"file"`
// 		FileProxy string `json:"fileProxy"`
// 	}

// 	bytecodeData, err := os.ReadFile("byteCode/byteCode.json")
// 	if err != nil {
// 		return common.Address{}, fmt.Errorf("failed to read bytecode file: %w", err)
// 	}

// 	var bytecodeFile BytecodeFile
// 	if err := json.Unmarshal(bytecodeData, &bytecodeFile); err != nil {
// 		return common.Address{}, fmt.Errorf("failed to parse bytecode file: %w", err)
// 	}

// 	// Parse bytecode (remove 0x prefix if present)
// 	bytecodeHex := strings.TrimPrefix(bytecodeFile.File, "0x")
// 	bytecode := common.FromHex(bytecodeHex)

// 	if len(bytecode) == 0 {
// 		return common.Address{}, fmt.Errorf("contract bytecode not available")
// 	}

// 	// Deploy the implementation contract
// 	address, tx, _, err := bind.DeployContract(auth, parsedABI, bytecode, client)
// 	if err != nil {
// 		return common.Address{}, fmt.Errorf("failed to deploy contract: %w", err)
// 	}

// 	// Wait for transaction receipt
// 	receipt, err := waitForTransaction(client, tx.Hash(), "Files Implementation")
// 	if err != nil {
// 		return common.Address{}, fmt.Errorf("transaction failed: %w", err)
// 	}

// 	log.Printf("✅ Implementation contract deployed at: %s", address.Hex())
// 	log.Printf("⛽ Gas used: %d", receipt.GasUsed)

// 	return address, nil
// }

// // deployFileProxy deploys the FileProxy contract with implementation address
// func deployFileProxy(client *ethclient.Client, auth *bind.TransactOpts, implementationAddress common.Address) (common.Address, error) {
// 	log.Println("Deploying FileProxy contract...")

// 	// Load proxy bytecode from byteCode/byteCode.json
// 	type BytecodeFile struct {
// 		File      string `json:"file"`
// 		FileProxy string `json:"fileProxy"`
// 	}

// 	bytecodeData, err := os.ReadFile("byteCode/byteCode.json")
// 	if err != nil {
// 		return common.Address{}, fmt.Errorf("failed to read bytecode file: %w", err)
// 	}

// 	var bytecodeFile BytecodeFile
// 	if err := json.Unmarshal(bytecodeData, &bytecodeFile); err != nil {
// 		return common.Address{}, fmt.Errorf("failed to parse bytecode file: %w", err)
// 	}

// 	// Parse proxy bytecode
// 	proxyBytecodeHex := strings.TrimPrefix(bytecodeFile.FileProxy, "0x")
// 	proxyBytecode := common.FromHex(proxyBytecodeHex)

// 	if len(proxyBytecode) == 0 {
// 		return common.Address{}, fmt.Errorf("proxy bytecode not available")
// 	}

// 	// Prepare constructor arguments: address _implementation, bytes memory _data
// 	// _data is empty (0x)
// 	proxyABI := `[{"inputs":[{"internalType":"address","name":"_implementation","type":"address"},{"internalType":"bytes","name":"_data","type":"bytes"}],"stateMutability":"nonpayable","type":"constructor"}]`
// 	parsedProxyABI, err := abi.JSON(strings.NewReader(proxyABI))
// 	if err != nil {
// 		return common.Address{}, fmt.Errorf("failed to parse proxy ABI: %w", err)
// 	}

// 	// Pack constructor arguments
// 	constructorArgs, err := parsedProxyABI.Pack("", implementationAddress, []byte{})
// 	if err != nil {
// 		return common.Address{}, fmt.Errorf("failed to pack constructor arguments: %w", err)
// 	}

// 	// Combine bytecode with constructor arguments
// 	fullBytecode := append(proxyBytecode, constructorArgs...)

// 	// Increment nonce for second transaction
// 	auth.Nonce = big.NewInt(0).Add(auth.Nonce, big.NewInt(1))

// 	// Deploy proxy contract
// 	tx := types.NewContractCreation(
// 		auth.Nonce.Uint64(),
// 		auth.Value,
// 		auth.GasLimit,
// 		auth.GasPrice,
// 		fullBytecode,
// 	)

// 	signedTx, err := auth.Signer(auth.From, tx)
// 	if err != nil {
// 		return common.Address{}, fmt.Errorf("failed to sign transaction: %w", err)
// 	}

// 	err = client.SendTransaction(context.Background(), signedTx)
// 	if err != nil {
// 		return common.Address{}, fmt.Errorf("failed to send transaction: %w", err)
// 	}

// 	// Wait for transaction receipt
// 	receipt, err := waitForTransaction(client, signedTx.Hash(), "FileProxy")
// 	if err != nil {
// 		return common.Address{}, fmt.Errorf("transaction failed: %w", err)
// 	}

// 	log.Printf("✅ Proxy contract deployed at: %s", receipt.ContractAddress.Hex())
// 	log.Printf("⛽ Gas used: %d", receipt.GasUsed)

// 	return receipt.ContractAddress, nil
// }

// // initializeContract calls the initialize function on the contract through proxy
// func initializeContract(client *ethclient.Client, auth *bind.TransactOpts, contract *bind.BoundContract) error {
// 	// Increment nonce for next transaction
// 	auth.Nonce = big.NewInt(0).Add(auth.Nonce, big.NewInt(1))

// 	// Call initialize function
// 	tx, err := contract.Transact(auth, "initialize")
// 	if err != nil {
// 		return fmt.Errorf("failed to send initialize transaction: %w", err)
// 	}

// 	log.Printf("📤 Initialize transaction sent: %s", tx.Hash().Hex())

// 	// Wait for transaction receipt
// 	receipt, err := waitForTransaction(client, tx.Hash(), "initialize")
// 	if err != nil {
// 		return fmt.Errorf("initialize transaction failed: %w", err)
// 	}

// 	log.Printf("⛽ Gas used: %d", receipt.GasUsed)
// 	return nil
// }

// // getDeployerAuth creates a transaction auth from private key
// func getDeployerAuth(client *ethclient.Client, privateKeyHex string) (*bind.TransactOpts, error) {
// 	// Remove 0x prefix if present
// 	privateKeyHex = strings.TrimPrefix(privateKeyHex, "0x")

// 	privateKey, err := crypto.HexToECDSA(privateKeyHex)
// 	if err != nil {
// 		return nil, fmt.Errorf("invalid private key: %w", err)
// 	}

// 	publicKey := privateKey.Public()
// 	publicKeyECDSA, ok := publicKey.(*ecdsa.PublicKey)
// 	if !ok {
// 		return nil, fmt.Errorf("error casting public key to ECDSA")
// 	}

// 	fromAddress := crypto.PubkeyToAddress(*publicKeyECDSA)

// 	nonce, err := client.PendingNonceAt(context.Background(), fromAddress)
// 	if err != nil {
// 		return nil, fmt.Errorf("failed to get nonce: %w", err)
// 	}
// 	if nonce != 1 {
// 		log.Printf("❌ LỖI: Nonce hiện tại của tài khoản %s là %d.", fromAddress.Hex(), nonce)
// 		return nil, fmt.Errorf("yêu cầu deploy thất bại: nonce phải là 1 (đang là %d)", nonce)
// 	}
// 	chainID, err := client.ChainID(context.Background())
// 	if err != nil {
// 		return nil, fmt.Errorf("failed to get chain ID: %w", err)
// 	}

// 	auth, err := bind.NewKeyedTransactorWithChainID(privateKey, chainID)
// 	if err != nil {
// 		return nil, fmt.Errorf("failed to create transactor: %w", err)
// 	}

// 	auth.Nonce = big.NewInt(int64(nonce))
// 	auth.Value = big.NewInt(0)
// 	auth.GasLimit = uint64(30000000)
// 	auth.GasPrice = nil // Use gas price suggestion

// 	return auth, nil
// }

// // waitForTransaction waits for a transaction to be mined
// func waitForTransaction(client *ethclient.Client, txHash common.Hash, contractName string) (*types.Receipt, error) {
// 	log.Printf("⏳ Waiting for %s deployment transaction to be mined...", contractName)

// 	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
// 	defer cancel()

// 	for {
// 		receipt, err := client.TransactionReceipt(ctx, txHash)
// 		if err == nil {
// 			if receipt.Status == 1 {
// 				return receipt, nil
// 			}
// 			return nil, fmt.Errorf("transaction failed with status %d", receipt.Status)
// 		}

// 		select {
// 		case <-ctx.Done():
// 			return nil, fmt.Errorf("timeout waiting for transaction")
// 		case <-time.After(2 * time.Second):
// 			// Continue waiting
// 		}
// 	}
// }

// // getEnv gets environment variable with default value
// func getEnv(key, defaultValue string) string {
// 	value := os.Getenv(key)
// 	if value == "" {
// 		return defaultValue
// 	}
// 	return value
// }

// // parseCommaSeparatedString parses comma-separated string into array
// func parseCommaSeparatedString(input string) []string {
// 	if input == "" {
// 		return []string{}
// 	}
// 	// Remove quotes and split by comma
// 	input = strings.Trim(input, "\"")
// 	parts := strings.Split(input, ",")
// 	result := make([]string, 0, len(parts))
// 	for _, part := range parts {
// 		trimmed := strings.TrimSpace(part)
// 		if trimmed != "" {
// 			result = append(result, trimmed)
// 		}
// 	}
// 	return result
// }

// // saveDeploymentInfo saves deployment information to a file
// func saveDeploymentInfo(info map[string]interface{}) {
// 	data, err := json.MarshalIndent(info, "", "  ")
// 	if err != nil {
// 		log.Printf("Warning: Failed to marshal deployment info: %v", err)
// 		return
// 	}

// 	filename := fmt.Sprintf("deployment_%s.json", time.Now().Format("20060102_150405"))
// 	if err := os.WriteFile(filename, data, 0644); err != nil {
// 		log.Printf("Warning: Failed to save deployment info: %v", err)
// 		return
// 	}

// 	log.Printf("\n💾 Deployment info saved to: %s", filename)
// }

// // setRustServerAddresses calls the setRustServerAddresses function on the contract
// func setRustServerAddresses(client *ethclient.Client, auth *bind.TransactOpts, contract *bind.BoundContract, addresses []string) error {
// 	// Increment nonce for next transaction
// 	auth.Nonce = big.NewInt(0).Add(auth.Nonce, big.NewInt(1))

// 	// Pack the function call
// 	tx, err := contract.Transact(auth, "setRustServerAddresses", addresses)
// 	if err != nil {
// 		return fmt.Errorf("failed to send transaction: %w", err)
// 	}
// 	// Wait for transaction receipt
// 	receipt, err := waitForTransaction(client, tx.Hash(), "setRustServerAddresses")
// 	if err != nil {
// 		return fmt.Errorf("transaction failed: %w", err)
// 	}

// 	log.Printf("⛽ Gas used: %d", receipt.GasUsed)
// 	return nil
// }

// // addStorageServer calls the addStorageServer function on the contract
// func addStorageServer(client *ethclient.Client, auth *bind.TransactOpts, contract *bind.BoundContract, storageServerAddress common.Address) error {
// 	// Increment nonce for next transaction
// 	auth.Nonce = big.NewInt(0).Add(auth.Nonce, big.NewInt(1))

// 	// Pack the function call
// 	tx, err := contract.Transact(auth, "addStorageServer", storageServerAddress)
// 	if err != nil {
// 		return fmt.Errorf("failed to send transaction: %w", err)
// 	}

// 	log.Printf("📤 Transaction sent: %s", tx.Hash().Hex())

// 	// Wait for transaction receipt
// 	receipt, err := waitForTransaction(client, tx.Hash(), "addStorageServer")
// 	if err != nil {
// 		return fmt.Errorf("transaction failed: %w", err)
// 	}

// 	log.Printf("⛽ Gas used: %d", receipt.GasUsed)
// 	return nil
// }
