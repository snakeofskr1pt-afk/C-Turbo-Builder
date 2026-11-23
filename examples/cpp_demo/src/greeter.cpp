#include "greeter.h"

#include <sstream>

namespace demo {

std::string greeting(const std::string& name) {
    return "Hello, " + name + "! Built by cpp-turbo-builder.";
}

std::string summary(long long sum, long long fib10) {
    std::ostringstream out;
    out << "sum(1..100) = " << sum << ", fib(10) = " << fib10;
    return out.str();
}

} // namespace demo
