#!/bin/bash
tmux kill-server
pkill -9 main  # Kill tiến trình Go
pkill -9 metanode # Kill tiến trình Rust
