Run the tree-sitter parse sweep first in the release grammar-audit lane, ahead of the metadata and
language-spec verification phases, so a grammar-parse regression surfaces in seconds after the grammar
dependencies install instead of minutes into the lane.
