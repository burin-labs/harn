#include "tree_sitter/parser.h"
#include "tree_sitter/alloc.h"

#include <stdbool.h>
#include <stdint.h>
#include <wctype.h>

enum TokenType {
  BLOCK_SEP,
  LINE_SEP,
};

typedef struct {
  uint16_t indent[64];
  uint8_t len;
} ScannerState;

static bool is_ignored_separator_target(int32_t lookahead) {
  switch (lookahead) {
    case '.':
    case '!':
    case '=':
    case '<':
    case '>':
    case '|':
    case '&':
    case '+':
    case '*':
    case '?':
      return true;
    default:
      return false;
  }
}

void *tree_sitter_harn_external_scanner_create(void) {
  ScannerState *state = ts_calloc(1, sizeof(ScannerState));
  return state;
}

void tree_sitter_harn_external_scanner_destroy(void *payload) {
  ts_free(payload);
}

unsigned tree_sitter_harn_external_scanner_serialize(void *payload, char *buffer) {
  ScannerState *state = (ScannerState *)payload;
  buffer[0] = (char)state->len;
  for (uint8_t i = 0; i < state->len; i++) {
    buffer[1 + (i * 2)] = (char)(state->indent[i] & 0xff);
    buffer[2 + (i * 2)] = (char)((state->indent[i] >> 8) & 0xff);
  }
  return 1 + (state->len * 2);
}

void tree_sitter_harn_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  ScannerState *state = (ScannerState *)payload;
  state->len = 0;
  if (length == 0) {
    return;
  }
  uint8_t len = (uint8_t)buffer[0];
  if (len > 64) {
    len = 64;
  }
  if (length < 1 + (len * 2)) {
    len = (length - 1) / 2;
  }
  state->len = len;
  for (uint8_t i = 0; i < state->len; i++) {
    uint8_t lo = (uint8_t)buffer[1 + (i * 2)];
    uint8_t hi = (uint8_t)buffer[2 + (i * 2)];
    state->indent[i] = (uint16_t)(lo | (hi << 8));
  }
}

bool tree_sitter_harn_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
  ScannerState *state = (ScannerState *)payload;

  if (!valid_symbols[BLOCK_SEP] && !valid_symbols[LINE_SEP]) {
    return false;
  }

  bool saw_newline = false;
  uint16_t indent = 0;

  while (true) {
    if (lexer->lookahead == '\r') {
      saw_newline = true;
      indent = 0;
      lexer->advance(lexer, true);
      if (lexer->lookahead == '\n') {
        lexer->advance(lexer, true);
      }
      continue;
    }
    if (lexer->lookahead == '\n') {
      saw_newline = true;
      indent = 0;
      lexer->advance(lexer, true);
      continue;
    }
    if ((lexer->lookahead == ' ' || lexer->lookahead == '\t') && saw_newline) {
      indent += lexer->lookahead == '\t' ? 2 : 1;
      lexer->advance(lexer, true);
      continue;
    }
    break;
  }

  if (!saw_newline) {
    return false;
  }

  if (is_ignored_separator_target(lexer->lookahead)) {
    return false;
  }

  if (valid_symbols[BLOCK_SEP]) {
    if (state->len == 0 || indent > state->indent[state->len - 1]) {
      if (state->len < 64) {
        state->indent[state->len++] = indent;
      } else {
        state->indent[state->len - 1] = indent;
      }
    } else if (state->len > 0) {
      state->indent[state->len - 1] = indent;
    }
    lexer->result_symbol = BLOCK_SEP;
    return true;
  }

  while (state->len > 0 && indent < state->indent[state->len - 1]) {
    state->len--;
  }

  if (state->len == 0 || indent == state->indent[state->len - 1]) {
    lexer->result_symbol = LINE_SEP;
    return true;
  }

  return false;
}
