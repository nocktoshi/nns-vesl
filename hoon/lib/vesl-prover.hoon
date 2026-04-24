::  lib/vesl-prover.hoon: STARK proof generation for arbitrary Nock computation
::
::  Forked from nock-prover.hoon to bypass puzzle-nock.  The standard
::  prover is PoW-specific: it derives [subject formula] from
::  puzzle-nock(header, nonce, pow-len).  Vesl needs to prove
::  arbitrary Nock computations (e.g. settle-note execution).
::
::  This prover:
::  1. Accepts [subject formula root hull] directly
::  2. Traces execution via fink:fock
::  3. Packs root/hull as tip5 digests into proof-stream header/nonce
::  4. Generates a STARK proof via the same constraint system
::
::  Phase 3: subject is belt-decomposed manifest data (all atoms < p).
::  Formula is a hand-crafted Nock 0-8 expression embedded in the
::  subject for self-reference via Nock 2 recursion.
::
/=  compute-table-v2  /common/v2/table/prover/compute
/=  memory-table-v2  /common/v2/table/prover/memory
/=  nock-common-v2  /common/v2/nock-common
/=  *  /common/zeke
/=  stark-prover  /common/stark/prover
/#  softed-constraints
::
|%
::
::  +prover-core: STARK prover initialised with softed constraints
::
++  prover-core
  =|  in=stark-input
  =/  sc=stark-config
    %*  .  *stark-config
      prep  softed-constraints
    ==
  %_    stark-prover
      +<+<
    %_  in
      stark-config  sc
    ==
  ==
::
::  +prove-computation: Generate STARK proof of any Nock [subject formula]
::
::  Bypasses puzzle-nock entirely.  The STARK constraints only check
::  correct Nock VM execution, not which program was run.
::
::  root and hull are packed into the proof stream's header and nonce
::  fields as tip5 digests.  These are Fiat-Shamir bound — modifying
::  them after proof generation invalidates all FRI challenges.
::
++  prove-computation
  |=  [subject=* formula=* root=@ hull=@]
  ^-  prove-result:stark-prover
  ::  1. trace the nock execution
  ::
  =/  [prod=* return=fock-return]
    (fink:fock [subject formula])
  ::  2. decompose root and hull into field-safe tip5 digests
  ::
  =/  root-digest=noun-digest:tip5  (atom-to-digest:tip5 root)
  =/  hull-digest=noun-digest:tip5  (atom-to-digest:tip5 hull)
  ::  3. extract v2 preprocessed constraints
  ::
  =/  pre=preprocess-data
    p.pre-2.softed-constraints
  ::  4. call generate-proof via prove-door directly from stark-prover
  ::
  ::  Bypass prover-core — the +<+< axis modification may corrupt
  ::  the core.  prove-door takes pre as an explicit argument anyway.
  ::
  ::  header = root digest, nonce = hull digest.
  ::  These become the first proof-stream push [%puzzle ...],
  ::  absorbed into the Fiat-Shamir sponge before challenge derivation.
  ::
  %-  %~  generate-proof
        prove-door:stark-prover
      :*  nock-common-v2
          funcs:compute-table-v2
          static:common:compute-table-v2
          funcs:memory-table-v2
          static:common:memory-table-v2
          pre
      ==
  :*  %2
      root-digest
      hull-digest
      0
      subject
      formula
      prod
      return
  ==
::
::  +prove-standard: pass-through to standard PoW prover
::
++  prove-standard
  |=  input=prover-input:stark-prover
  (prove:prover-core input)
--
