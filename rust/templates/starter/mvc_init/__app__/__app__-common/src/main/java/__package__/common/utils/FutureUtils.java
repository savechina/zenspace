package __package__.common.utils;

import java.util.List;
import java.util.Objects;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.Executor;
import java.util.concurrent.TimeUnit;

/**
 * Future工具类
 *
 * @author weirenyan
 * @date 2023/09/26
 */
public class FutureUtils {

    /**
     * 异步对象
     */
    public static CompletableFuture<Void> runAsync(Runnable runnable, Executor executor) {
        return CompletableFuture.runAsync(runnable, executor);
    }

    /**
     * 获取异步结果
     *
     * @param futures 异步对象
     * @param timeout 超时时间，单位毫秒
     */
    public static void allOfGet(List<CompletableFuture<Void>> futures, int timeout) throws Exception {
        CompletableFuture
                .allOf(futures.stream().filter(Objects::nonNull).toArray(CompletableFuture[]::new))
                .get(timeout, TimeUnit.MILLISECONDS);
    }

}
