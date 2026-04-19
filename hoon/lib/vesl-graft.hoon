::  lib/vesl-graft.hoon: gate-agnostic composable verification for any NockApp
::
::  The Graft is a library, not a kernel. It provides:
::    1. A state fragment (vesl-state) you graft onto your kernel state
::    2. A poke dispatcher for %vesl-register/%vesl-settle/%vesl-verify
::    3. A peek helper for querying registered/settled status
::
::  The caller passes a verify-gate — a function that takes opaque data
::  and an expected root, returns a loobean. RAG is one implementation.
::  Any computation type works. No domain-specific imports.
::
::  Usage:
::    /+  *vesl-graft
::    /+  *rag-logic    :: for RAG gate (or your own domain logic)
::    ...your kernel...
::    +$  my-state  [vesl=vesl-state ...your-fields...]
::    ...in poke arm...
::    =/  my-gate=verify-gate
::      |=  [data=* expected-root=@]
::      (verify-manifest ;;(manifest data) expected-root)
::    ?+  -.cause  [~ state]
::      %vesl-register  (vesl-poke vesl.state cause my-gate)
::      %vesl-settle    (vesl-poke vesl.state cause my-gate)
::      %vesl-verify    (vesl-poke vesl.state cause my-gate)
::    ==
::
|%
::  +$vesl-state: the state fragment — graft this onto your kernel
::
::  registered: hull-id -> merkle-root
::  settled: set of note IDs (replay protection)
::
+$  vesl-state
  $:  registered=(map @ @)
      settled=(set @)
  ==
::
::  +$graft-payload: generic settlement payload
::
::  note: the settlement note header (id, hull, root, pending state)
::  data: opaque — the verification gate knows the shape.
::       for RAG, this is a manifest. for other domains, anything.
::  expected-root: the Merkle root this data is bound to
::
+$  graft-payload
  $:  note=[id=@ hull=@ root=@ state=[%pending ~]]
      data=*
      expected-root=@
  ==
::
::  +$verify-gate: domain verification gate signature
::
::  Takes opaque data + expected root, returns loobean.
::  The gate casts data to its domain type (e.g., ;;(manifest data))
::  and performs domain-specific verification.
::
+$  verify-gate  $-([data=* expected-root=@] ?)
::
::  +$vesl-effect: effects the Graft can produce
::
+$  vesl-effect
  $%  [%vesl-registered hull=@ root=@]
      [%vesl-settled note=[id=@ hull=@ root=@ state=[%settled ~]]]
      [%vesl-verified ok=?]
      [%vesl-error msg=@t]
  ==
::
::  +$vesl-cause: tagged pokes the Graft handles
::
+$  vesl-cause
  $%  [%vesl-register hull=@ root=@]
      [%vesl-settle payload=@]
      [%vesl-verify payload=@]
  ==
::
::  +vesl-poke: dispatch a vesl cause against vesl state
::
::  Takes a verify-gate as third argument — the caller's domain
::  verification function.  Returns [effects updated-state].
::
++  vesl-poke
  |=  [state=vesl-state cause=vesl-cause veri=verify-gate]
  ^-  [(list vesl-effect) vesl-state]
  ?-  -.cause
    ::
    ::  %vesl-register — store hull root
    ::
      %vesl-register
    ::  Guard: reject re-registration (hull already has a root)
    ::
    ?:  (~(has by registered.state) hull.cause)
      :_  state
      ~[[%vesl-error 'vesl-graft: hull already registered']]
    =/  new-reg  (~(put by registered.state) hull.cause root.cause)
    :_  state(registered new-reg)
    ~[[%vesl-registered hull.cause root.cause]]
    ::
    ::  %vesl-settle — cue payload, validate, verify via gate, settle
    ::    Guards: root registered, roots match, replay protection, set cap
    ::    Crash semantics: ?> on gate failure = unprovable STARK
    ::
      %vesl-settle
    =/  raw=*  (cue payload.cause)
    =/  args=graft-payload  ;;(graft-payload raw)
    ::  Guard: reject unregistered roots
    ::
    ?.  (~(has by registered.state) hull.note.args)
      :_  state
      ~[[%vesl-error 'vesl-graft: root not registered']]
    ::  Guard: expected root must match registered root
    ::
    ?.  =(expected-root.args (~(got by registered.state) hull.note.args))
      :_  state
      ~[[%vesl-error 'vesl-graft: root mismatch']]
    ::  Guard: note header root must match expected root
    ::
    ?.  =(root.note.args expected-root.args)
      :_  state
      ~[[%vesl-error 'vesl-graft: note root does not match expected root']]
    ::  Guard: replay protection
    ::
    ?:  (~(has in settled.state) id.note.args)
      :_  state
      ~[[%vesl-error 'vesl-graft: note already settled']]
    ::  Guard: settled set capacity (V-002)
    ::
    ?:  (gte ~(wyt in settled.state) 1.000.000)
      :_  state
      ~[[%vesl-error 'vesl-graft: settled set at capacity']]
    ::  Verify via caller's gate — crash on failure
    ::
    ?>  (veri data.args expected-root.args)
    ::  Settle — transition to %settled
    ::
    =/  new-settled  (~(put in settled.state) id.note.args)
    :_  state(settled new-settled)
    ~[[%vesl-settled note=[id.note.args hull.note.args root.note.args [%settled ~]]]]
    ::
    ::  %vesl-verify — pure verification, no state transition
    ::    Returns [%vesl-verified %.y] or [%vesl-verified %.n].
    ::
      %vesl-verify
    =/  raw=*  (cue payload.cause)
    =/  args=graft-payload  ;;(graft-payload raw)
    ::  Check registration
    ::
    ?.  (~(has by registered.state) hull.note.args)
      :_  state
      ~[[%vesl-verified %.n]]
    ::  Guard: expected root must match registered root
    ::
    ?.  =(expected-root.args (~(got by registered.state) hull.note.args))
      :_  state
      ~[[%vesl-verified %.n]]
    ::  Verify via caller's gate — soft failure (no crash)
    ::
    =/  ok=?  (veri data.args expected-root.args)
    :_  state
    ~[[%vesl-verified ok]]
  ==
::
::  +vesl-peek: query vesl state by path
::
::  Returns ~ for unrecognized paths (pass through to your kernel's peek).
::  Returns ``(unit) for recognized paths.
::
++  vesl-peek
  |=  [state=vesl-state =path]
  ^-  (unit (unit *))
  ?+  path  ~
    [%registered hull=@ ~]
      =/  vid  +<.path
      ``(~(has by registered.state) vid)
    ::
    [%settled note-id=@ ~]
      =/  nid  +<.path
      ``(~(has in settled.state) nid)
    ::
    [%root hull=@ ~]
      =/  vid  +<.path
      ``(~(get by registered.state) vid)
  ==
--
