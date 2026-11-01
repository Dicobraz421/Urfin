; XML: теги — keyword, значения атрибутов и CDATA — string, ссылки на символы — number.
(Comment) @comment
(STag ["<" ">"] @keyword (Name) @keyword)
(ETag ["</" ">"] @keyword (Name) @keyword)
(EmptyElemTag ["<" "/>"] @keyword (Name) @keyword)
(Attribute (AttValue) @string)
(CDSect) @string
[(CharRef) (EntityRef)] @number
(XMLDecl) @keyword
(PI) @keyword
(doctypedecl) @keyword
