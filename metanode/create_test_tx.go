package main

import (
	"fmt"
	"os"
	"encoding/binary"
	
	"github.com/meta-node-blockchain/meta-node/cmd/simple_chain/pkg/proto/transaction"
)

func main() {
	// Create a simple transaction with just data
	tx := &transaction.Transaction{
		Data: []byte("test_transaction_" + fmt.Sprintf("%d", time.Now().Unix())),
		FromAddress: []byte("test_from"),
		ToAddress: []byte("test_to"),
		Amount: []byte("100"),
		Nonce: []byte("1"),
		MaxGas: 21000,
		MaxGasPrice: 1000000000,
	}
	
	// Create Transactions message
	txs := &transaction.Transactions{
		Transactions: []*transaction.Transaction{tx},
	}
	
	// Marshal to protobuf
	data, err := txs.Marshal()
	if err != nil {
		fmt.Printf("Error marshaling: %v\n", err)
		os.Exit(1)
	}
	
	// Write length prefix + data
	var buf [4]byte
	binary.BigEndian.PutUint32(buf[:], uint32(len(data)))
	
	// Write to stdout as hex for the shell script
	fmt.Printf("%x", buf[:])
	for _, b := range data {
		fmt.Printf("%02x", b)
	}
	fmt.Println()
}
