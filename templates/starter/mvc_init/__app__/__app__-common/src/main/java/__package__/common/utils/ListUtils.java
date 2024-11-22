package __package__.common.utils;

import java.util.ArrayList;
import java.util.List;
import java.util.function.Consumer;

import static java.util.Optional.ofNullable;

/**
 * 集合工具类
 *
 * @author weirenyan
 * @since 2023/04/20
 */
public class ListUtils {

    /**
     * 均分list, 注意方法返回的顺序不和源list保持一致
     * <p>
     * Lists.partition
     *
     * @param list 需要均分的list
     * @param size 每个分组的最大数量
     * @return 分组结果
     * @see com.google.common.collect.Lists
     */
    public static <T> List<List<T>> partition(List<T> list, int size) {
        // 确认查询多少次
        int page = list.size() / size + (list.size() % size == 0 ? 0 : 1);

        // 需要把sku平分到每个组里面
        List<List<T>> partitions = new ArrayList<>(page);
        for (int i = 0; i < page; i++) {
            partitions.add(new ArrayList<>(size));
        }

        int i = 0;
        for (T item : list) {
            partitions.get(i++ % page).add(item);
        }

        return partitions;
    }

    /**
     * list foreach
     * <p>
     * Lists.foreach
     *
     * @see com.google.common.collect.Lists
     */
    public static <T> void foreach(List<T> list, Consumer<? super T> action) {
        if (null == list || list.isEmpty()) {
            return;
        }
        list.forEach(action);
    }

    /**
     * 返回两个集合的组合，集合可以为空
     *
     * @param <T> the element type
     * @param list1  the first list
     * @param list2  the second list
     * @return a new list containing the union of those lists
     */
    public static <T> List<T> union(List<T> list1, List<T> list2) {
        List<T> l1 = ofNullable(list1).orElse(new ArrayList<>(0));
        List<T> l2 = ofNullable(list2).orElse(new ArrayList<>(0));
        final ArrayList<T> result = new ArrayList<>(l1.size() + l2.size());
        result.addAll(l1);
        result.addAll(l2);
        return result;
    }

}
