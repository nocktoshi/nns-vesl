::  lib/tx-witness.hoon — narrow Nockchain page / tx-set helpers for NNS.
::
::  Duplicates the *hashing* shape of `page:tx-engine-1` / `z-set` walks
::  from `nockchain/hoon/common/tx-engine-{0,1}.hoon` without importing the
::  full tx-engine cone (see `scripts/setup-hoon-tree.sh`).
::
::  Included today:
::    - `++block-commitment` — same field order as `+hashable-block-commitment`
::      on a v1 page body (parent, tx-ids, coinbase, timestamp, epoch-counter,
::      target, accumulated-work, height, msg). `target` / `accumulated-work`
::      are opaque nouns (`*`) so we stay aligned with whatever bignum shape
::      the chain jams.
::    - `++has-tx-in-ids` — `~(has z-in …)` over a `(z-set tx-id)`.
::
::  Intentional non-goals:
::    - Do not vendor `++spends` / `++outputs` or rebuild transaction
::      outputs here. Nockchain already executes and validates
::      transactions; NNS should not duplicate that state transition.
::    - Only add more helpers when they bind commitments or membership
::      (for example, hashing an already-provided raw transaction to a
::      tx-id, if the recursive proof needs that binding).
::
/=  *  /common/zoon
|%
+$  hash  [@ux @ux @ux @ux @ux]
+$  tx-id  hash
+$  coins  @ud
+$  page-number  @ud
::
::  v1 coinbase split: lock-hash -> coins (matches `coinbase-split:page:t`).
::
+$  coinbase-split-v1  (z-map hash coins)
::
::  Minimal v1 page *tail* used for block commitment (everything after pow).
::
+$  page-commit-tail
  $:  parent=hash
      tx-ids=(z-set tx-id)
      coinbase=coinbase-split-v1
      timestamp=@
      epoch-counter=@ud
      target=*
      accumulated-work=*
      height=page-number
      msg=*
  ==
::
++  hashable-tx-ids
  |=  tx-ids=(z-set tx-id)
  ^-  hashable:tip5:z
  ?~  tx-ids  leaf+tx-ids
  :+  hash+n.tx-ids
    $(tx-ids l.tx-ids)
  $(tx-ids r.tx-ids)
::
++  hashable-coinbase-split-v1
  |=  form=coinbase-split-v1
  ^-  hashable:tip5:z
  ?~  form  leaf+form
  :+  [hash+p.n.form leaf+q.n.form]
    $(form l.form)
  $(form r.form)
::
++  hashable-block-commitment
  |=  =page-commit-tail
  ^-  hashable:tip5:z
  :*  hash+parent.page-commit-tail
      hash+(hash-hashable:tip5:z (hashable-tx-ids tx-ids.page-commit-tail))
      hash+(hash-hashable:tip5:z (hashable-coinbase-split-v1 coinbase.page-commit-tail))
      leaf+timestamp.page-commit-tail
      leaf+epoch-counter.page-commit-tail
      leaf+target.page-commit-tail
      leaf+accumulated-work.page-commit-tail
      leaf+height.page-commit-tail
      leaf+msg.page-commit-tail
  ==
::
::  +block-commitment: Tip5 digest of the block body commitment (no pow).
::
++  block-commitment
  |=  =page-commit-tail
  ^-  noun-digest:tip5:z
  (hash-hashable:tip5:z (hashable-block-commitment page-commit-tail))
::
::  +has-tx-in-ids: membership in the canonical tx-id z-set.
::
++  has-tx-in-ids
  |=  [tx-ids=(z-set tx-id) tid=tx-id]
  ^-  ?
  (~(has z-in tx-ids) tid)
::
::  +digest-to-ux: flatten a Tip5 digest to a single atom (matches
::  `digest-to-atom:tip5` use sites in the Rust hull).
::
++  digest-to-ux
  |=  d=noun-digest:tip5:z
  ^-  @ux
  (digest-to-atom:tip5:z d)
--