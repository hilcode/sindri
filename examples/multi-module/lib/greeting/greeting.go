package greeting

import "fmt"

// Message returns a friendly greeting for the given name.
func Message(name string) string {
	return fmt.Sprintf("Hello, %s!", name)
}
