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

static void skip_horizontal_space(TSLexer *lexer) {
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t') {
    lexer->advance(lexer, true);
  }
}

static bool consume_comment(TSLexer *lexer) {
  if (lexer->lookahead != '/') {
    return false;
  }
  lexer->advance(lexer, true);

  if (lexer->lookahead == '/') {
    lexer->advance(lexer, true);
    while (lexer->lookahead != 0 && lexer->lookahead != '\n' && lexer->lookahead != '\r') {
      lexer->advance(lexer, true);
    }
    return true;
  }

  if (lexer->lookahead != '*') {
    return false;
  }

  lexer->advance(lexer, true);
  uint16_t depth = 1;
  while (depth > 0 && lexer->lookahead != 0) {
    if (lexer->lookahead == '/') {
      lexer->advance(lexer, true);
      if (lexer->lookahead == '*') {
        lexer->advance(lexer, true);
        depth++;
      }
      continue;
    }
    if (lexer->lookahead == '*') {
      lexer->advance(lexer, true);
      if (lexer->lookahead == '/') {
        lexer->advance(lexer, true);
        depth--;
      }
      continue;
    }
    lexer->advance(lexer, true);
  }

  return true;
}

static bool consume_newline_run(TSLexer *lexer, uint16_t *indent) {
  bool saw_newline = false;
  *indent = 0;
  while (true) {
    if (lexer->lookahead == '\r') {
      saw_newline = true;
      *indent = 0;
      lexer->advance(lexer, true);
      if (lexer->lookahead == '\n') {
        lexer->advance(lexer, true);
      }
      continue;
    }
    if (lexer->lookahead == '\n') {
      saw_newline = true;
      *indent = 0;
      lexer->advance(lexer, true);
      continue;
    }
    if ((lexer->lookahead == ' ' || lexer->lookahead == '\t') && saw_newline) {
      *indent += lexer->lookahead == '\t' ? 2 : 1;
      lexer->advance(lexer, true);
      continue;
    }
    break;
  }
  return saw_newline;
}

static bool comment_line_continues_expression(TSLexer *lexer) {
  if (!consume_comment(lexer)) {
    return false;
  }

  skip_horizontal_space(lexer);
  uint16_t indent = 0;
  if (!consume_newline_run(lexer, &indent)) {
    return false;
  }

  skip_horizontal_space(lexer);
  while (consume_comment(lexer)) {
    skip_horizontal_space(lexer);
    if (!consume_newline_run(lexer, &indent)) {
      return false;
    }
    skip_horizontal_space(lexer);
  }

  return is_ignored_separator_target(lexer->lookahead);
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
  unsigned required = 1u + ((unsigned)len * 2u);
  if (length < required) {
    len = (uint8_t)((length - 1u) / 2u);
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

  uint16_t indent = 0;
  bool saw_newline = consume_newline_run(lexer, &indent);

  if (!saw_newline) {
    return false;
  }

  lexer->mark_end(lexer);

  if (comment_line_continues_expression(lexer) || is_ignored_separator_target(lexer->lookahead)) {
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
