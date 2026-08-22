# Report Go sources whose formatting differs from gofmt's, without rewriting them, so a mis-formatted
# file fails the build rather than being silently changed.
fun inputs => [ { program = "gofmt", arguments = [ "-l", "." ] } ]
