interface Speaker {
    fun speak(): String
}

class Greeter(val name: String) : Speaker {
    override fun speak(): String {
        return "hello $name"
    }

    fun shout(message: String): String {
        return message.uppercase()
    }
}

object Defaults {
    fun world(): Greeter = Greeter("world")
}

fun globalShout(s: String): String = s.uppercase()
