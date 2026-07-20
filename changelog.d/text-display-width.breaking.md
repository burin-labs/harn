- `word_wrap` and the PDF document renderer (`document_render_pdf`) now measure
  text in terminal **display columns** instead of grapheme/character counts.
  Wide glyphs — CJK ideographs and most emoji — count as two columns and
  combining marks as zero, matching how the text actually occupies a line. For
  ASCII input the measurement is unchanged (a column equals a character), but
  wrapping of non-ASCII text now breaks in different, correct places.
