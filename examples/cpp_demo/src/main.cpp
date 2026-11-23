#include "greeter.h"
#include "math_utils.h"

#include <iostream>

int main() {
    const long long sum = demo::sum_to(100);
    const long long fib10 = demo::fib(10);

    std::cout << demo::greeting("developer") << '\n';
    std::cout << demo::summary(sum, fib10) << '\n';
    return 0;
}
