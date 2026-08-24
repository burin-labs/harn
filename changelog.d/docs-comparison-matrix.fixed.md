**`npm run dev` renders the docs site again.** The client entry hydrated
whenever the root element had any child node, and in development the unreplaced
`<!--app-html-->` placeholder is itself a child, so React hydrated against
markup that was not there and left a blank page. It now tests for an element.
The Python comparison snippet in `why-harn.md` called `harness.stdio.log`, a
Harn builtin, instead of `print`, and the Harn snippet beside it carried an
unused parameter.
