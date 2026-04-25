package fixture

type Greeter struct {
	Name string
}

type Speaker interface {
	Speak() string
}

func (g Greeter) Greet() string {
	return "hello " + g.Name
}

func Shout(s string) string {
	return s
}

func TestSomething() {}
