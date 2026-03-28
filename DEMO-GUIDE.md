# Two-Phone Consensus Demo Guide

Step-by-step instructions to demonstrate multi-node consensus between two Android phones on the same WiFi network.

## Prerequisites

- Two Android phones with Gratia app installed:
  - Samsung Galaxy A06 (serial: R9ZX90L1F2W)
  - Samsung Galaxy S25 (serial: RFCXC0N7ZCE)
- Both phones on the same WiFi network
- USB cable for adb (or wireless adb)
- Build environment set up per `scripts/build-android.sh`

## Step 1: Build and Deploy

```bash
cd "C:\Users\Michael\Desktop\Project GRATIA\gratia"

# Build the Rust native library (debug for faster iteration)
./scripts/build-android.sh debug

# Build and install the Android APK on both phones
cd app/android
./gradlew installDebug

# Verify both phones have the app
adb -s R9ZX90L1F2W shell pm list packages | grep gratia
adb -s RFCXC0N7ZCE shell pm list packages | grep gratia
```

## Step 2: Get Phone IP Addresses

Both phones must be on the same WiFi network.

```bash
# Phone A (A06)
adb -s R9ZX90L1F2W shell ip addr show wlan0 | grep "inet "
# Note the IP, e.g., 192.168.1.100

# Phone B (S25)
adb -s RFCXC0N7ZCE shell ip addr show wlan0 | grep "inet "
# Note the IP, e.g., 192.168.1.101
```

## Step 3: Launch and Monitor Logs

Open two terminal windows for log monitoring:

```bash
# Terminal 1: Phone A logs
adb -s R9ZX90L1F2W logcat -s GratiaCore:V GratiaFFI:V *:S

# Terminal 2: Phone B logs
adb -s RFCXC0N7ZCE logcat -s GratiaCore:V GratiaFFI:V *:S
```

## Step 4: Start the App on Both Phones

1. Open Gratia on Phone A
2. Open Gratia on Phone B
3. Both should show the Wallet screen with a generated address

## Step 5: Start Network Layer

On each phone, navigate to the **Network** screen and tap **Start Network**.

**Expected logs on each phone:**
```
FFI: network started on port 0
```

The port 0 means the OS picked a random available port.

## Step 6: Connect Phones to Each Other

On Phone A's Network screen, tap **Connect Peer** and enter Phone B's address:
```
/ip4/192.168.1.101/udp/9000/quic-v1
```

(Replace the IP and port with Phone B's actual values. The port is logged when the network starts.)

**Expected logs on Phone A:**
```
FFI: dialing peer at /ip4/192.168.1.101/udp/9000/quic-v1
```

**Expected logs on Phone B:**
```
PeerConnected { peer_id: "..." }
```

Both phones should show `1 peer` in the Network screen.

## Step 7: Start Consensus

On each phone, navigate to the **Mining** screen and tap **Start Mining** (or trigger consensus start from settings).

**Expected logs:**
```
Trust-aware committee pool breakdown (30+ day threshold)
FFI: consensus started
Slot timer: this node should produce a block
Block produced (demo mode)
Block broadcast to network
Block finalized — mining reward credited
```

## Step 8: Verify Multi-Node Consensus

Watch the logs on both phones. You should see:

**On the block producer:**
```
Block produced (demo mode)
Block broadcast to network
Block finalized — mining reward credited
```

**On the other phone:**
```
Processed incoming block from network
```

With graduated committee scaling at 21 nodes (the demo pads to 21), the phones take turns producing blocks based on VRF selection. Each 4-second slot, one of the committee members is selected as block producer.

## Troubleshooting

### "network not started" error when starting consensus
Start the network BEFORE starting consensus. The network layer must be running to receive blocks.

### Phones don't discover each other
- Verify both are on the same WiFi network
- Check that no firewall/AP isolation is blocking UDP traffic
- Try the explicit connect_peer approach instead of mDNS discovery

### No "Block broadcast to network" log
- Check that the network has at least 1 connected peer
- The broadcast uses `try_broadcast_block_sync` — if the log says "Failed to broadcast block", the gossipsub channel may be full. This resolves itself on the next slot.

### "Failed to process incoming block" on receiving phone
- The block validation failed. Check the error message:
  - "Block timestamp is in the future" — clock skew between phones. Ensure both phones have accurate time (NTP sync).
  - "Block height mismatch" — the receiving phone is out of sync. It needs the blocks it missed first (sync not yet implemented for Phase 1).

### Only one phone produces blocks
This is expected if the VRF selection for the current epoch favors one phone's committee position. The demo epoch lasts ~1 hour (900 slots × 4 seconds). Committee rotation happens at epoch boundaries.

## What Success Looks Like

- Both phones show increasing block height in Mining screen
- Both phones show blocks being received from each other in Network screen
- Wallet balance increases on the producing phone (50 GRAT per block)
- Logs show "Block broadcast to network" and "Processed incoming block from network" alternating between phones

## Known Limitations (Phase 1 Demo)

1. **Single-node committee in practice:** The current `start_consensus` bootstraps a committee where only the local node is "real" and 20 are synthetic. For a true multi-phone committee, both phones' node IDs need to be in each other's eligible node lists. This requires a committee exchange protocol (Phase 2).

2. **No state sync:** If one phone starts later or misses blocks, it can't catch up. There's no block sync protocol wired to the FFI yet.

3. **No block persistence:** Blocks are kept in memory only. Restarting the app loses all state.

4. **Auto-finalization:** Blocks are auto-finalized by the producer (no real committee signing). In production, 14/21 committee signatures are required.

## Next Steps After Demo

1. **Committee exchange:** Phones share their EligibleNode info over the network so both appear in each other's committees
2. **Block sync protocol:** Catch-up mechanism for phones that missed blocks
3. **RocksDB persistence:** Survive app restarts
4. **Real committee signing:** Collect signatures from committee members before finalization
