#!/bin/bash
# The links walk-through's data on the local stack (docs/LINKS_CONTRACT.md, wave 4).
set -e
cd "$(dirname "$0")/../.."
scripts/local-stack/up.sh
scripts/local-stack/desk.sh owner 17491; scripts/local-stack/desk.sh bob 17492; scripts/local-stack/desk.sh carol 17493
sleep 2
. scripts/local-stack/env.sh
H=scripts/local-stack/signup-helper/target/release/signup-helper
for who in alice bob carol; do $H $EDGE $who@example.com "pw-$who-0123456789"; done
grep -o "$EDGE[^ \"]*verify[^ \"]*" $STACK/cloud.log | sort -u | xargs -n1 curl -s -o /dev/null -L
PIMBLE_CLOUD_PASSWORD=pw-alice-0123456789 own cloud-sign-in $EDGE alice@example.com
PIMBLE_CLOUD_PASSWORD=pw-bob-0123456789 bob cloud-sign-in $EDGE bob@example.com
PIMBLE_CLOUD_PASSWORD=pw-carol-0123456789 carol cloud-sign-in $EDGE carol@example.com
mkdir -p $STACK/owner
id(){ grep -oE '[0-9a-f-]{36}' | head -1; }
root(){ grep -oE '[0-9a-f-]{36}' | tail -1; }
out=$(own create-store $STACK/owner/family.pimble Family); F=$(echo "$out" | id); FR=$(echo "$out" | root)
out=$(own create-store $STACK/owner/work.pimble Work); W=$(echo "$out" | id); WR=$(echo "$out" | root)
nid(){ grep -oE '[0-9a-f-]{36}' | tail -1; }
HOL=$(own create-node $F $FR folder Holiday | nid); PRI=$(own create-node $F $FR folder Private | nid)
TRAIN=$(own create-node $F $HOL document Train | nid); PACK=$(own create-node $F $HOL document Packing | nid)
DIARY=$(own create-node $F $PRI document Diary | nid); OLD=$(own create-node $F $HOL document Scrap | nid)
PLAN=$(own create-node $W $WR document Plan | nid)
own set-node-text $F $TRAIN "Train leaves at nine from platform four." >/dev/null
own set-node-text $F $PACK "Packing list for the trip." >/dev/null
own set-node-text $F $DIARY "Private diary entry." >/dev/null
own set-node-text $F $OLD "A scrap." >/dev/null
own set-node-text $W $PLAN "Work plan." >/dev/null
own append-link $W $PLAN "pimble:$F/$TRAIN" holiday train
own append-link $W $PLAN "pimble:$F/$DIARY" the diary
own create-mount $W $WR $F $HOL "Holiday (mounted)"
own cloud-host-store $F; sleep 3
own cloud-share $F $HOL --name "Holiday plans"
own cloud-share-invite $F $HOL bob@example.com editor
own cloud-share-invite $F $HOL carol@example.com editor
sleep 2
bob cloud-add-hosted $F; carol cloud-add-hosted $F; sleep 6
HOTEL=$(bob create-node $F $HOL document Hotel | nid); LINKS=$(bob create-node $F $HOL document Links | nid)
text=$(for i in $(seq 1 40); do if [ $i = 30 ]; then echo "Paragraph $i. The hotel key code is 4827 at the side door, ask for Marta."; else echo "Paragraph $i of the hotel notes, with enough words to fill a line or so of the editor."; fi; done)
bob set-node-text $F $HOTEL "$text" >/dev/null
bob set-node-text $F $LINKS "Links to try." >/dev/null
bob append-link $F $LINKS "pimble:$F/$TRAIN" moved out of the share
bob append-link $F $LINKS "pimble:$F/$OLD" deleted scrap
bob append-link $F $LINKS "pimble:$F/$PACK" moved to another store
bob append-link $F $LINKS "pimble:$F/$DIARY" not shared
bob append-link $F $LINKS "pimble:$F/$HOTEL" the hotel
bob append-link $F $LINKS "https://example.com/" a web page
sleep 2
own move-node $F $TRAIN $PRI
bob delete-node $F $OLD
own transplant-node $F $PACK $W $WR
cat > $STACK/ids.sh <<IDS
F=$F; FR=$FR; W=$W; WR=$WR; HOL=$HOL; PRI=$PRI; TRAIN=$TRAIN; PACK=$PACK; DIARY=$DIARY; OLD=$OLD; PLAN=$PLAN; HOTEL=$HOTEL; LINKS=$LINKS
IDS
cat $STACK/ids.sh
