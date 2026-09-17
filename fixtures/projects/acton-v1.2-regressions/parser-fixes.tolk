enum Mode { First = 1; Second = 2; Third, Fourth; }
type Empty = [];
type Nested = [[]];
type Choice = (| int | bool);
const ESCAPED_END = """a\"""";
const ESCAPED_MIDDLE = """a\"""b""";
const ESCAPED_ONE_QUOTE = """a"\"b""";
const ESCAPED_TWO_QUOTES = """a""\"b""";
