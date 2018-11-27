#!/usr/bin/env bash
rm -rf test.db
diesel migration run

cargo build
RUST_LOG=bounty_hunter::network_endpoints=trace cargo run &

sleep 0.1
cat test.json | http --verify no POST https://localhost:4878/upload_channel_state

http --verify no GET https://localhost:4878/get_channel_state/0x0000000000000000000000000000000000000001

killall bounty_hunter
