::  tests/names.hoon — compile-time tests for nns-vesl's
::  verification gate.
::
::  The kernel's sole job is to evaluate the gate defined in
::  hoon/app/app.hoon. These tests re-implement the gate's helpers
::  in parallel (kept trivially short so any drift is obvious) and
::  assert the guarantees (G1 format + G2 Merkle inclusion) across
::  representative batches — including the K-leaf shape the hull
::  hands to the graft.
::
::  Compile: hoonc --new --arbitrary hoon/tests/names.hoon hoon/
::  Success (build succeeded) = all assertions passed.
::
/+  *vesl-merkle
::
=>
|%
++  assert-eq
  |*  [a=* b=*]
  ~|  'assert-eq: values not equal'
  ?>  =(a b)
  %.y
::
::  gate helpers — parallel copies of those in hoon/app/app.hoon
::
++  valid-char
  |=  c=@  ^-  ?
  ?|  &((gte c 'a') (lte c 'z'))
      &((gte c '0') (lte c '9'))
  ==
::
++  all-valid-chars
  |=  cord=@t  ^-  ?
  =/  n  (met 3 cord)
  =/  i=@  0
  |-
  ?:  =(i n)  %.y
  ?.  (valid-char (cut 3 [i 1] cord))  %.n
  $(i +(i))
::
++  has-nock-suffix
  |=  cord=@t  ^-  ?
  =/  n  (met 3 cord)
  ?:  (lth n 6)  %.n
  =((cut 3 [(sub n 5) 5] cord) '.nock')
::
++  stem-len
  |=  cord=@t  ^-  @ud
  (sub (met 3 cord) 5)
::
++  is-valid-name
  |=  name=@t  ^-  ?
  ?.  (has-nock-suffix name)  %.n
  =/  slen  (stem-len name)
  ?:  =(slen 0)  %.n
  (all-valid-chars (cut 3 [0 slen] name))
::
++  fee-for
  |=  name=@t  ^-  @ud
  =/  slen  (stem-len name)
  ?:  =(slen 0)  0
  ?:  (gte slen 10)  6.553.600
  ?:  (gte slen 5)   32.768.000
  327.680.000
::
::  Merkle primitives — parallel copies.
::
++  nth
  |=  [lst=(list @) i=@ud]
  ^-  @
  ?~  lst  ~|('nth: out of bounds' !!)
  ?:  =(i 0)  i.lst
  $(lst t.lst, i (dec i))
::
++  next-level
  |=  level=(list @)
  ^-  (list @)
  ?~  level  ~
  ?~  t.level
    ~[(hash-pair i.level i.level)]
  [(hash-pair i.level i.t.level) $(level t.t.level)]
::
++  compute-root
  |=  leaves=(list @)
  ^-  @
  ?~  leaves  0
  =/  level  (turn leaves hash-leaf)
  |-  ^-  @
  ?:  ?=([@ ~] level)  i.level
  $(level (next-level level))
::
++  proof-for
  |=  [leaves=(list @) idx=@ud]
  ^-  (list [hash=@ side=?])
  =/  level=(list @)  (turn leaves hash-leaf)
  =|  acc=(list [hash=@ side=?])
  =/  i=@ud  idx
  |-  ^-  (list [hash=@ side=?])
  ?:  ?=([@ ~] level)  (flop acc)
  =/  n=@ud  (lent level)
  =/  sibling-idx=@ud
    ?:  =(0 (mod i 2))  +(i)
    (sub i 1)
  =/  sib=@
    ?:  (lth sibling-idx n)  (nth level sibling-idx)
    (nth level i)
  =/  side=?  =(1 (mod i 2))
  %=  $
    level  (next-level level)
    i      (div i 2)
    acc    [[sib side] acc]
  ==
::
::  nns-gate under test — batch shape.
::  data = (list [name owner tx-hash proof]); every leaf must clear
::  G1 (name format) and G2 (Merkle inclusion under expected-root).
::
++  nns-gate
  |=  [data=* expected-root=@]
  ^-  ?
  =/  leaves
    ;;((list [name=@t owner=@t tx-hash=@t proof=(list [hash=@ side=?])]) data)
  |-  ^-  ?
  ?~  leaves  %.y
  =/  chunk=@  (jam [name.i.leaves owner.i.leaves tx-hash.i.leaves])
  ?&  (is-valid-name name.i.leaves)
      (verify-chunk chunk proof.i.leaves expected-root)
      $(leaves t.leaves)
  ==
--
::
::  ============================================
::  FIXTURES
::  ============================================
::
=/  alice=@t  'alice-address'
=/  bob=@t    'bob-address'
=/  tx1=@t    'tx-hash-1'
=/  tx2=@t    'tx-hash-2'
=/  tx3=@t    'tx-hash-3'
::
::  ============================================
::  G1: name format
::  ============================================
::
?>  (assert-eq (is-valid-name 'a.nock') %.y)
?>  (assert-eq (is-valid-name 'abc123.nock') %.y)
?>  (assert-eq (is-valid-name 'deadbeef01.nock') %.y)
?>  (assert-eq (is-valid-name '.nock') %.n)
?>  (assert-eq (is-valid-name 'foo') %.n)
?>  (assert-eq (is-valid-name 'foo.bar') %.n)
?>  (assert-eq (is-valid-name 'Foo.nock') %.n)
?>  (assert-eq (is-valid-name 'foo-bar.nock') %.n)
?>  (assert-eq (is-valid-name 'foo.nock.nock') %.y)
?>  (assert-eq (is-valid-name 'foo_bar.nock') %.n)
::
::  ============================================
::  Fee tiers match legacy worker (nicks)
::  ============================================
::
?>  (assert-eq (fee-for 'a.nock') 327.680.000)
?>  (assert-eq (fee-for 'abcd.nock') 327.680.000)
?>  (assert-eq (fee-for 'abcde.nock') 32.768.000)
?>  (assert-eq (fee-for 'abcdefghi.nock') 32.768.000)
?>  (assert-eq (fee-for 'abcdefghij.nock') 6.553.600)
::
::  ============================================
::  Batch G2: 1-leaf batch (smallest real case)
::  ============================================
::
=/  leaf-a=@             (jam ['alpha.nock' alice tx1])
=/  leaves-1=(list @)    ~[leaf-a]
=/  root-1=@             (compute-root leaves-1)
=/  proof-1=(list [hash=@ side=?])  (proof-for leaves-1 0)
::
?>  %-  assert-eq
    :-  %.y
    %-  nns-gate
    :_  root-1
    ~[[name='alpha.nock' owner=alice tx-hash=tx1 proof=proof-1]]
::
::  ============================================
::  Batch G2: 3-leaf batch (every leaf at every position).
::  Also exercises the duplicate-last padding at odd levels.
::  ============================================
::
=/  leaf-al=@  (jam ['alpha.nock' alice tx1])
=/  leaf-br=@  (jam ['bravo.nock' bob tx2])
=/  leaf-ch=@  (jam ['charlie.nock' alice tx3])
=/  leaves-3=(list @)  ~[leaf-al leaf-br leaf-ch]
=/  root-3=@           (compute-root leaves-3)
=/  proof-al=(list [hash=@ side=?])  (proof-for leaves-3 0)
=/  proof-br=(list [hash=@ side=?])  (proof-for leaves-3 1)
=/  proof-ch=(list [hash=@ side=?])  (proof-for leaves-3 2)
::
?>  %-  assert-eq
    :-  %.y
    %-  nns-gate
    :_  root-3
    ^-  (list [name=@t owner=@t tx-hash=@t proof=(list [hash=@ side=?])])
    :~  [name='alpha.nock' owner=alice tx-hash=tx1 proof=proof-al]
        [name='bravo.nock' owner=bob tx-hash=tx2 proof=proof-br]
        [name='charlie.nock' owner=alice tx-hash=tx3 proof=proof-ch]
    ==
::
::  ============================================
::  Batch G2: subset batches verify independently
::  ============================================
::
?>  %-  assert-eq
    :-  %.y
    %-  nns-gate
    :_  root-3
    ^-  (list [name=@t owner=@t tx-hash=@t proof=(list [hash=@ side=?])])
    :~  [name='alpha.nock' owner=alice tx-hash=tx1 proof=proof-al]
        [name='charlie.nock' owner=alice tx-hash=tx3 proof=proof-ch]
    ==
::
?>  %-  assert-eq
    :-  %.y
    %-  nns-gate
    :_  root-3
    ~[[name='bravo.nock' owner=bob tx-hash=tx2 proof=proof-br]]
::
::  ============================================
::  Batch rejects: one tampered leaf poisons the whole batch
::  ============================================
::
?>  %-  assert-eq
    :-  %.n
    %-  nns-gate
    :_  root-3
    ^-  (list [name=@t owner=@t tx-hash=@t proof=(list [hash=@ side=?])])
    :~  [name='alpha.nock' owner=alice tx-hash=tx1 proof=proof-al]
        [name='bravo.nock' owner=alice tx-hash=tx2 proof=proof-br]
        [name='charlie.nock' owner=alice tx-hash=tx3 proof=proof-ch]
    ==
::
::  ============================================
::  Batch rejects: proof/leaf mismatch (proof for index 0 on leaf
::  that actually lives at index 1)
::  ============================================
::
?>  %-  assert-eq
    :-  %.n
    %-  nns-gate
    :_  root-3
    ~[[name='bravo.nock' owner=bob tx-hash=tx2 proof=proof-al]]
::
::  ============================================
::  Batch rejects: format failure (invalid name in the batch)
::  ============================================
::
=/  bad-leaf=@  (jam ['Bad.nock' alice tx1])
=/  bad-root=@  (compute-root ~[bad-leaf])
=/  bad-proof=(list [hash=@ side=?])  (proof-for ~[bad-leaf] 0)
?>  %-  assert-eq
    :-  %.n
    %-  nns-gate
    :_  bad-root
    ~[[name='Bad.nock' owner=alice tx-hash=tx1 proof=bad-proof]]
::
::  Mixing a well-formed leaf with a malformed one also fails.
::
?>  %-  assert-eq
    :-  %.n
    %-  nns-gate
    :_  root-1
    ^-  (list [name=@t owner=@t tx-hash=@t proof=(list [hash=@ side=?])])
    :~  [name='alpha.nock' owner=alice tx-hash=tx1 proof=proof-1]
        [name='Bad.nock' owner=alice tx-hash=tx1 proof=~]
    ==
::
::  ============================================
::  Batch rejects: root mismatch (valid leaf against a different
::  commitment)
::  ============================================
::
?>  %-  assert-eq
    :-  %.n
    %-  nns-gate
    :_  (compute-root ~[(jam ['other.nock' alice tx1])])
    ~[[name='alpha.nock' owner=alice tx-hash=tx1 proof=proof-1]]
::
::  ============================================
::  Empty batch: vacuously accepted by the gate itself.
::  The kernel's %settle-batch arm rejects empty batches at the
::  layer above, but the gate has no reason to fail on "nothing
::  to disprove" — `(list)` == `~` is a valid value for `data`.
::  ============================================
::
?>  %-  assert-eq
    :-  %.y
    %-  nns-gate
    [~ root-1]
::
%pass
