; Markdown (блоки): заголовки и маркеры — keyword, код — string, служебное — comment.
[(atx_heading) (setext_heading)] @keyword
[(list_marker_minus) (list_marker_plus) (list_marker_star) (list_marker_dot) (list_marker_parenthesis)] @keyword
[(task_list_marker_checked) (task_list_marker_unchecked)] @keyword
(fenced_code_block (fenced_code_block_delimiter) @comment)
(info_string) @keyword
[(code_fence_content) (indented_code_block)] @string
(block_quote_marker) @comment
(thematic_break) @comment
(pipe_table_delimiter_row) @comment
(html_block) @comment
(link_reference_definition (link_label) @keyword)
(link_destination) @string
[(minus_metadata) (plus_metadata)] @comment
