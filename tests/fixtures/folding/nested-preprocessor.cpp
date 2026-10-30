#ifndef FOLD_CACHE
#define FOLD_CACHE 1
#endif
#ifndef FOLD_FAST_PATH
#define FOLD_FAST_PATH 0
#endif

#define FOLD_ASSERT(condition) \
    do { \
        if (!(condition)) { \
            return -1; \
        } \
    } while (false)

namespace sample {
    struct Processor
    {
        int run(int limit)
        {
            int total = 0;
#if FOLD_CACHE
            if (limit > 0) {
                for (int i = 0; i < limit; ++i)
                {
                    if (i % 2 == 0) {
#ifdef _WIN32
                        if (i > 4) {
                            total += i;
                        } // platform_limit
                        else
                        {
                            total += 2;
                        } // small_platform_value
#else // platform
                        total += i * 2;
#endif // platform
                    } // even
                    else if (i > 3) {
                        FOLD_ASSERT(total >= 0);
                        total -= 1;
                    }
                } // loop
            } // positive_limit
#elif FOLD_FAST_PATH
            total = limit * 2;
#else // cache_fallback
            if (limit != 0) {
                total = 1;
            }
#endif // cache

            if (total < 0)
            {
#define FOLD_NONZERO(value) \
                ((value) != 0)
#ifndef FOLD_IGNORE_NEGATIVE
                while (FOLD_NONZERO(total)) {
                    if (total < -10) {
                        total += 2;
                    } else {
                        ++total;
                    }
                } // negative_loop
#endif // negative
#undef FOLD_NONZERO
            } // negative_total
            return total;
        } // run

        int fallback(int value) {
#if defined(FOLD_CACHE) && !FOLD_FAST_PATH
            if (value > 0)
            {
                return value;
            } // fallback_positive
#else // fallback
            if (value < 0) {
                return -value;
            }
#endif // fallback
            return 0;
        } // fallback_method
    }; // Processor
} // namespace
