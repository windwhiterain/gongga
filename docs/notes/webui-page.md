# webui/page.rs

The page is embedded with `include_str!`: the binary is the page, so a host started
from anywhere serves what it was built with and cannot be pointed at a checkout's
files. The route table is exact -- the eight files and `/` for `index.html`, with
`/favicon.ico` answered 204 -- because the page names its own modules and a path the
table does not name is a 404, not a directory walk.

Every response carries `Cache-Control: no-store`: a rebuilt host must be a new page.
The MIME types are stated here rather than guessed from a name, and the JavaScript ones
are `text/javascript` because the browser refuses an ES module served as
`text/plain`.
