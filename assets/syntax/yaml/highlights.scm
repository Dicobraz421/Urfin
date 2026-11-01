; YAML: ключи — keyword (это структура документа), строки — string.
(comment) @comment
(block_mapping_pair key: (_) @keyword)
(flow_pair key: (_) @keyword)
[(double_quote_scalar) (single_quote_scalar) (block_scalar) (string_scalar)] @string
[(integer_scalar) (float_scalar) (boolean_scalar) (null_scalar) (timestamp_scalar)] @number
[(anchor) (alias) (tag)] @keyword
[(yaml_directive) (tag_directive) (reserved_directive)] @keyword
["---" "..."] @comment
