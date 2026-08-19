The llama.cpp Qwen3.6 route now uses the provider's native tool channel. Its
capability row previously declared that the model does not return parseable
native tool calls, which a fresh forced-format sweep falsified.

The prior row held `native_tools = false` pending "a receipted forced-format
family sweep". That sweep ran on the same quant the previous receipt measured:
six coding-agent fixtures, two replicates, forced through native, tagged text,
and fenced JSON. Native produced 58 successful dispatches including real file
edits and command runs, with the plain tools array confirmed on the wire, so
the `text_only` parity verdict is retired.

The default moved to native on a completion TIE, not a completion win, and the
receipt says so. Two-sided Fisher separates nothing between the channels
(native vs JSON p=0.474, native vs tagged text p=1.000). What separates them is
cost per turn: the text channel has to teach its dialect in every request, which
spent about 7,000 characters of a 65,536-token context window per turn against
the same tools declared natively, alongside 33% less wall time and 21% fewer
iterations for native.

Two supporting fixes ride along. The `no-tool-diagnosis` fixture graded a
free-form diagnosis by substring, so it failed answers that were correct but
phrased differently; it now accepts the forms a correct diagnosis actually
takes while still requiring both sides of the operator swap. And the local audit
gate fanout was missing two stdlib gates that CI runs, so a green local sweep
said nothing about them.
