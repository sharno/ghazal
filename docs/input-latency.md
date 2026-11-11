# Button latency

The H700 handhelds' buttons are GPIO pins read by ROCKNIX's
`rocknix-singleadc-joypad` driver, which also reads the analog sticks through
the ADC. Upstream, it reads every button on the same 10 ms poll as the sticks
(`poll-interval = <10>` in the device trees), so a press waits 0 to 10 ms
(5 ms on average) before anything sees it. At 60 fps that moves about a third
of presses to a later frame.

Ghazal's kernel patches the driver (`nix/rocknix-joypad-button-irqs.patch`):
each button pin raises an interrupt on both edges and its change is reported
at once. Every pin of the H700's GPIO banks A and C to I can raise one, so all
buttons on every board use it; a pin that cannot falls back to the poll with a
`has no interrupt; polling it` warning in `dmesg`. The sticks stay on the 10 ms
poll, and the input device keeps its name, IDs and button codes, so RetroArch's
profile and SDL mappings are unchanged.

## Bounce

A polled button never sees contact bounce: 10 ms between reads is longer than a
switch bounces. Read on its edge, a bounce would arrive as extra presses. After
any edge a button settles for 5 ms: it ignores further edges and then reads the
pin again, reporting a change it finds (a release within the 5 ms, or an edge
lost while the driver briefly switched the pin to an input to read it) and
settling again. A press is reported on its first edge; only a release inside
the settling time waits for its end.

## Checking it on a handheld

```sh
dmesg | grep -i 'joypad\|polling it'   # no "polling it" lines
grep joypad /proc/interrupts            # one "joypad <label>" line per button
ghazal observe buttons 30               # then press buttons for 30 s
```

`observe buttons` reads the pad's events and prints the presses, any bounces (a
button changing again within 15 ms) and the share of edges that fall in the
busiest 2 ms of each 10 ms. Polled edges share one phase of the poll, so close
to 100% of them land there; edge-driven ones spread over the 10 ms, about 20%.
Expect 0 bounces when pressing at a normal pace; run it again with fast taps and
rolls across the D-pad, and check a game and the menu for doubled inputs.
