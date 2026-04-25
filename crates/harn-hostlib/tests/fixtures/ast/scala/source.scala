trait Speaker {
  def speak(): String
}

case class Greeter(name: String) extends Speaker {
  def speak(): String = s"hello $name"
}

object Defaults {
  val World: Greeter = Greeter("world")
}

def shout(s: String): String = s.toUpperCase
