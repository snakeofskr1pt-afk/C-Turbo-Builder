#include "math_utils.h"

namespace demo {

long long sum_to(long long n) {
    long long total = 0;
    for (long long i = 1; i <= n; ++i) {
        total += i;
    }
    return total;
}

long long fib(int n) {
    if (n < 2) {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

} // namespace demo
