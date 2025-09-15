package __package__.common.convert;

import java.util.List;

/**
 * 对象互转模板类
 *
 * @author weirenyan
 */
public interface EntityConverter<Entity, DO> {

    /**
     * 类型转换
     *
     * @param entity 输入
     * @return 输出
     */
    DO convert(Entity entity);

    /**
     * 类型转换
     *
     * @param entities 输入
     * @return 输出
     */
    List<DO> batchConvert(List<Entity> entities);

    /**
     * 类型转换
     *
     * @param entity 输入
     * @return 输出
     */
    Entity reverseConvert(DO entity);

    /**
     * 类型转换
     *
     * @param entities 输入
     * @return 输出
     */
    List<Entity> batchReverseConvert(List<DO> entities);


}
